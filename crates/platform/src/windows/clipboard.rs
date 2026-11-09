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
