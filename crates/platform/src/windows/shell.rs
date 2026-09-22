use anyhow::{Result, bail};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{HSTRING, w};

/// 用系统默认程序打开 URL
pub fn open_url(url: &str) -> Result<()> {
    let target = HSTRING::from(url);
    let r = unsafe { ShellExecuteW(None, w!("open"), &target, None, None, SW_SHOWNORMAL) };
    if r.0 as isize <= 32 {
        bail!("打开链接失败: {url}");
    }
    Ok(())
}
