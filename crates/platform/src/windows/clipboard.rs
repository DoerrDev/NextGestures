use std::time::Duration;

use anyhow::{Result, bail};
use windows::Win32::Foundation::{HANDLE, HGLOBAL};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, GetClipboardData, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};

const CF_UNICODETEXT: u32 = 13;

fn open() -> Result<()> {
    open_for(None)
}

fn open_for(owner: Option<windows::Win32::Foundation::HWND>) -> Result<()> {
    let mut last = windows::core::Error::empty();
    for _ in 0..10 {
        match unsafe { OpenClipboard(owner) } {
            Ok(()) => return Ok(()),
            Err(e) => last = e,
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    bail!("访问剪贴板失败（锁屏或其他程序占用）：{last}")
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
    set_text_for_window(text, 0)
}

pub fn set_text_for_window(text: &str, owner: isize) -> Result<()> {
    let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    open_for((owner != 0).then_some(windows::Win32::Foundation::HWND(owner as *mut _)))?;
    let out = (|| -> Result<()> {
        unsafe {
            EmptyClipboard()?;
            let mem = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2)?;
            let p = GlobalLock(mem) as *mut u16;
            if p.is_null() {
                let _ = windows::Win32::Foundation::GlobalFree(Some(mem));
                bail!("剪贴板分配失败");
            }
            std::ptr::copy_nonoverlapping(wide.as_ptr(), p, wide.len());
            let _ = GlobalUnlock(mem);
            if let Err(e) = SetClipboardData(CF_UNICODETEXT, Some(HANDLE(mem.0))) {
                let _ = windows::Win32::Foundation::GlobalFree(Some(mem));
                return Err(e.into());
            }
            Ok(())
        }
    })();
    unsafe { CloseClipboard() }.ok();
    out
}

/// CF_DIB 使用 BGRA、自底向上的像素排列，兼容画图、聊天软件等粘贴目标。
fn image_dib(frame: &super::screen::Frame) -> Result<Vec<u8>> {
    anyhow::ensure!(frame.width > 0 && frame.height > 0 && u64::from(frame.width) * u64::from(frame.height) * 4 == frame.rgba.len() as u64, "图片像素长度错误");
    let mut dib = Vec::with_capacity(40 + frame.rgba.len());
    dib.extend_from_slice(&40u32.to_le_bytes());
    dib.extend_from_slice(&(frame.width as i32).to_le_bytes());
    dib.extend_from_slice(&(frame.height as i32).to_le_bytes());
    dib.extend_from_slice(&1u16.to_le_bytes());
    dib.extend_from_slice(&32u16.to_le_bytes());
    dib.extend_from_slice(&[0u8; 24]);
    for row in frame.rgba.chunks_exact(frame.width as usize * 4).rev() {
        for p in row.chunks_exact(4) { dib.extend_from_slice(&[p[2], p[1], p[0], p[3]]); }
    }
    Ok(dib)
}

pub fn set_image(frame: &super::screen::Frame, owner: isize) -> Result<()> {
    use windows::Win32::Foundation::GlobalFree;
    let dib = image_dib(frame)?;
    open_for(Some(windows::Win32::Foundation::HWND(owner as *mut _)))?;
    let out = (|| -> Result<()> {
        unsafe {
            let mem = GlobalAlloc(GMEM_MOVEABLE, dib.len())?;
            let target = GlobalLock(mem);
            if target.is_null() { let _ = GlobalFree(Some(mem)); bail!("剪贴板分配失败"); }
            std::ptr::copy_nonoverlapping(dib.as_ptr(), target as *mut u8, dib.len());
            let _ = GlobalUnlock(mem);
            if let Err(e) = EmptyClipboard().and_then(|_| SetClipboardData(8, Some(HANDLE(mem.0))).map(|_| ())) {
                let _ = GlobalFree(Some(mem));
                return Err(e.into());
            }
        }
        Ok(())
    })();
    unsafe { CloseClipboard() }.ok();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn clipboard_dib_has_windows_header_and_bottom_up_bgra_pixels() {
        let frame = super::super::screen::Frame { width: 1, height: 2, rgba: vec![255, 0, 0, 255, 0, 0, 255, 255] };
        let dib = image_dib(&frame).unwrap();
        assert_eq!(u32::from_le_bytes(dib[0..4].try_into().unwrap()), 40);
        assert_eq!(i32::from_le_bytes(dib[8..12].try_into().unwrap()), 2);
        assert_eq!(&dib[40..44], &[255, 0, 0, 255]);
        assert_eq!(&dib[44..48], &[0, 0, 255, 255]);
        assert!(image_dib(&super::super::screen::Frame { width: 0, height: 1, rgba: vec![] }).is_err());
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
