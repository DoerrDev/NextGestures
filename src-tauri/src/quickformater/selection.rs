use std::sync::Mutex;
use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, ensure};
use gesture_platform::input;
use windows::Win32::Foundation::{GlobalFree, HANDLE, HGLOBAL, HWND};
use windows::Win32::System::Variant::VT_BOOL;
use windows::Win32::System::{Com::*, DataExchange::*, Memory::*};
use windows::Win32::UI::{Accessibility::*, Input::KeyboardAndMouse::*, WindowsAndMessaging::*};
use windows::core::{Interface, w};

const UNICODE_TEXT: u32 = 13;
static CLIPBOARD_ACCESS: Mutex<()> = Mutex::new(());

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Target {
    foreground: usize,
    focus: usize,
}

impl Target {
    pub fn current() -> Option<Self> {
        unsafe {
            let foreground = GetForegroundWindow();
            if foreground.0.is_null() {
                return None;
            }
            let thread = GetWindowThreadProcessId(foreground, None);
            let mut info = GUITHREADINFO {
                cbSize: size_of::<GUITHREADINFO>() as u32,
                ..Default::default()
            };
            GetGUIThreadInfo(thread, &mut info).ok()?;
            Some(Self {
                foreground: foreground.0 as usize,
                focus: info.hwndFocus.0 as usize,
            })
        }
    }

    pub fn foreground(self) -> usize {
        self.foreground
    }

    pub fn is_current(self) -> bool {
        Self::current() == Some(self)
    }

    fn owns_clipboard(self) -> bool {
        unsafe {
            let Ok(owner) = GetClipboardOwner() else {
                return false;
            };
            let (mut owner_pid, mut foreground_pid, mut focus_pid) = (0, 0, 0);
            GetWindowThreadProcessId(owner, Some(&mut owner_pid));
            GetWindowThreadProcessId(HWND(self.foreground as _), Some(&mut foreground_pid));
            if self.focus != 0 {
                GetWindowThreadProcessId(HWND(self.focus as _), Some(&mut focus_pid));
            }
            owner_pid != 0 && (owner_pid == foreground_pid || owner_pid == focus_pid)
        }
    }
}

pub fn editable(target: Target) -> bool {
    if !target.is_current() {
        return false;
    }
    if let Some(editable) = native_editability(HWND(target.focus as _)) {
        return editable;
    }
    // 可访问性提供程序可能跨进程阻塞。超时则展示结果供手动复制。
    let (send, receive) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let result = target.is_current() && inspect_editability().unwrap_or(false);
        let _ = send.send(result && target.is_current());
    });
    receive
        .recv_timeout(Duration::from_millis(700))
        .unwrap_or(false)
}

fn native_editability(focus: HWND) -> Option<bool> {
    let mut name = [0; 64];
    let len = unsafe { GetClassNameW(focus, &mut name) };
    if len <= 0 {
        return None;
    }
    let class = String::from_utf16_lossy(&name[..len as usize]).to_ascii_lowercase();
    if class == "edit" || class.starts_with("richedit") {
        let style = unsafe { GetWindowLongW(focus, GWL_STYLE) };
        return Some(style & (ES_READONLY | ES_PASSWORD) == 0);
    }
    None
}

fn inspect_editability() -> Option<bool> {
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED).ok().ok()?;
        struct Com;
        impl Drop for Com {
            fn drop(&mut self) {
                unsafe { CoUninitialize() }
            }
        }
        let _com = Com;
        let uia: IUIAutomation =
            CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).ok()?;
        let focused = uia.GetFocusedElement().ok()?;
        if focused.CurrentIsPassword().ok()?.as_bool() {
            return Some(false);
        }
        if let Ok(pattern) = focused.GetCurrentPattern(UIA_ValuePatternId)
            && let Ok(value) = pattern.cast::<IUIAutomationValuePattern>()
        {
            return value.CurrentIsReadOnly().ok().map(|value| !value.as_bool());
        }
        // RichEdit 和部分浏览器编辑器将只读标记暴露在 TextPattern 的选区上。
        let range_read_only = (|| {
            let pattern = focused.GetCurrentPattern(UIA_TextPatternId).ok()?;
            let text = pattern.cast::<IUIAutomationTextPattern>().ok()?;
            let range = text.GetSelection().ok()?.GetElement(0).ok()?;
            let attr = range.GetAttributeValue(UIA_IsReadOnlyAttributeId).ok()?;
            if attr.Anonymous.Anonymous.vt != VT_BOOL {
                return None;
            }
            bool::try_from(&attr).ok()
        })();
        if let Some(read_only) = range_read_only {
            return Some(!read_only);
        }
        // Some custom editors expose the Edit role but no Value or Text pattern.
        Some(focused.CurrentControlType().ok()? == UIA_EditControlTypeId)
    }
}

struct Clipboard;

impl Clipboard {
    fn open(owner: HWND) -> Result<Self> {
        for _ in 0..12 {
            if unsafe { OpenClipboard(Some(owner)) }.is_ok() {
                return Ok(Self);
            }
            sleep(Duration::from_millis(15));
        }
        anyhow::bail!("剪贴板正被占用，请稍后重试")
    }

    fn snapshot(&self) -> Result<Snapshot> {
        let mut saved = Vec::new();
        let mut remaining = 64 * 1024 * 1024;
        unsafe {
            let mut format = EnumClipboardFormats(0);
            while format != 0 {
                // GDI/私有句柄不能按字节复制。位图还有可完整保存的 DIB 格式。
                if !matches!(format, 2 | 3 | 9 | 14 | 0x80..=0x8f | 0x200..=0x3ff) {
                    let memory = HGLOBAL(GetClipboardData(format)?.0);
                    let len = GlobalSize(memory);
                    ensure!(len <= remaining, "当前剪贴板内容过大，请先清理后重试");
                    let bytes = GlobalLock(memory);
                    ensure!(!bytes.is_null(), "读取当前剪贴板内容失败");
                    let data = std::slice::from_raw_parts(bytes.cast::<u8>(), len).to_vec();
                    let _ = GlobalUnlock(memory);
                    saved.push((format, data));
                    remaining -= len;
                }
                format = EnumClipboardFormats(format);
            }
        }
        Ok(Snapshot(saved))
    }

    fn read_text(&self) -> Option<String> {
        unsafe {
            let memory = HGLOBAL(GetClipboardData(UNICODE_TEXT).ok()?.0);
            let len = GlobalSize(memory) / size_of::<u16>();
            let text = GlobalLock(memory).cast::<u16>();
            if text.is_null() {
                return None;
            }
            let units = std::slice::from_raw_parts(text, len);
            let end = units.iter().position(|unit| *unit == 0).unwrap_or(len);
            let value = String::from_utf16_lossy(&units[..end]);
            let _ = GlobalUnlock(memory);
            Some(value)
        }
    }

    fn put(&self, format: u32, bytes: &[u8]) -> Result<()> {
        unsafe {
            let memory = GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1))?;
            let data = GlobalLock(memory).cast::<u8>();
            if data.is_null() {
                let _ = GlobalFree(Some(memory));
                anyhow::bail!("分配剪贴板内存失败");
            }
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), data, bytes.len());
            let _ = GlobalUnlock(memory);
            if let Err(error) = SetClipboardData(format, Some(HANDLE(memory.0))) {
                let _ = GlobalFree(Some(memory));
                return Err(error.into());
            }
        }
        Ok(())
    }

    fn write_text(&self, text: &str, temporary: bool) -> Result<()> {
        let normalized = text.replace("\r\n", "\n").replace('\n', "\r\n");
        let bytes: Vec<_> = normalized
            .encode_utf16()
            .chain([0])
            .flat_map(u16::to_le_bytes)
            .collect();
        unsafe { EmptyClipboard()? };
        self.put(UNICODE_TEXT, &bytes)?;
        if temporary {
            self.exclude_from_history();
        }
        Ok(())
    }

    fn exclude_from_history(&self) {
        let format =
            unsafe { RegisterClipboardFormatW(w!("ExcludeClipboardContentFromMonitorProcessing")) };
        if format != 0 {
            let _ = self.put(format, &[0; 4]);
        }
    }
}

impl Drop for Clipboard {
    fn drop(&mut self) {
        let _ = unsafe { CloseClipboard() };
    }
}

struct Snapshot(Vec<(u32, Vec<u8>)>);

impl Snapshot {
    fn restore(self, clipboard: &Clipboard) -> Result<()> {
        unsafe { EmptyClipboard()? };
        for (format, data) in self.0 {
            clipboard.put(format, &data)?;
        }
        clipboard.exclude_from_history();
        Ok(())
    }
}

fn sequence() -> u32 {
    unsafe { GetClipboardSequenceNumber() }
}

fn modifiers_released() -> bool {
    let deadline = Instant::now() + Duration::from_millis(700);
    loop {
        if ![VK_SHIFT, VK_CONTROL, VK_MENU, VK_LWIN, VK_RWIN]
            .iter()
            .any(|key| unsafe { GetAsyncKeyState(i32::from(key.0)) } < 0)
        {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        sleep(Duration::from_millis(15));
    }
}

pub fn capture(owner: usize, target: Target) -> Result<String> {
    let _access = CLIPBOARD_ACCESS
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    ensure!(
        modifiers_released() && target.is_current(),
        "选区所在窗口已切换或修饰键仍按下"
    );
    let owner = HWND(owner as _);
    let (snapshot, original_sequence) = {
        let clipboard = Clipboard::open(owner)?;
        (clipboard.snapshot()?, sequence())
    };
    ensure!(
        target.is_current() && sequence() == original_sequence,
        "取词过程中窗口或剪贴板已变化"
    );
    input::send_hotkey(&["ctrl".into(), "c".into()])?;
    let deadline = Instant::now() + Duration::from_secs(1);
    while Instant::now() < deadline {
        if sequence() != original_sequence {
            // 等待目标程序提交 Unicode 和富文本格式。
            sleep(Duration::from_millis(40));
            let clipboard = Clipboard::open(owner)?;
            ensure!(target.owns_clipboard(), "剪贴板已被其他程序更新，请重试");
            let text = clipboard.read_text();
            snapshot.restore(&clipboard)?;
            ensure!(target.is_current(), "取词过程中窗口已切换");
            return text
                .filter(|text| !text.trim().is_empty())
                .context("没有选中可复制的文字");
        }
        sleep(Duration::from_millis(10));
    }
    anyhow::bail!("没有选中可复制的文字")
}

pub fn replace(
    owner: usize,
    target: Target,
    original: &str,
    result: &str,
    active: impl Fn() -> bool,
) -> Result<bool> {
    if !target.is_current()
        || !editable(target)
        || capture(owner, target).as_deref().ok() != Some(original)
    {
        return Ok(false);
    }
    if !active() || !modifiers_released() || !target.is_current() {
        return Ok(false);
    }
    let _access = CLIPBOARD_ACCESS
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if !target.is_current() {
        return Ok(false);
    }
    let owner = HWND(owner as _);
    let (snapshot, paste_sequence) = {
        let clipboard = Clipboard::open(owner)?;
        let snapshot = clipboard.snapshot()?;
        if let Err(error) = clipboard.write_text(result, true) {
            let _ = snapshot.restore(&clipboard);
            return Err(error);
        }
        (snapshot, sequence())
    };
    let paste = if active() && target.is_current() && sequence() == paste_sequence {
        input::send_hotkey(&["ctrl".into(), "v".into()]).map(|()| true)
    } else {
        Ok(false)
    };
    if paste.as_ref().is_ok_and(|sent| *sent) {
        sleep(Duration::from_millis(500));
    }
    let clipboard = Clipboard::open(owner)?;
    if sequence() == paste_sequence {
        snapshot.restore(&clipboard)?;
    }
    paste
}

pub fn copy_result(owner: usize, text: &str) -> Result<()> {
    let _access = CLIPBOARD_ACCESS
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    Clipboard::open(HWND(owner as _))?.write_text(text, false)
}
