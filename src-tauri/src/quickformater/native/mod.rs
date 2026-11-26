//! Native QuickFormater UI port. Its own Win32 message loop keeps the
//! spinner, streamed results, and transient input hooks off the Tauri thread.
// The native painting code follows the original Win32 unsafe function style.
#![allow(unsafe_op_in_unsafe_fn)]

mod anchor;
mod drawing;
mod hooks;
mod popup;
mod spinner;
mod theme;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use anyhow::{Result, ensure};
use gesture_core::AiSettings;
use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::{HiDpi::*, WindowsAndMessaging::*};

use super::{Mode, clean_output, endpoint, request, selection};
use popup::{Kind, Options};

const WM_APP_UPDATE: u32 = WM_APP + 1;
const WM_APP_ESC: u32 = WM_APP + 4;
const WM_APP_OUTSIDE: u32 = WM_APP + 5;
const WM_APP_WHEEL: u32 = WM_APP + 10;

fn utf16(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

enum Update {
    Delta(String),
    Done(Result<Completion>),
}

enum Completion {
    Card(Kind, String, u32),
    Silent,
}

/// `title` 为空时用 Kind 自带的标题（提示 / 遇到问题…）
fn card(anchor: RECT, kind: Kind, title: &str, text: &str, estimate: Option<&str>, auto_close_ms: u32) {
    popup::open(
        anchor,
        Options {
            kind,
            title,
            text,
            estimate,
            streaming: estimate.is_some(),
            font_size: 10.5,
            auto_close_ms,
        },
    );
}

/// 处理 AI 回复：开启说明时拆出 `<notes>`；替换模式先尝试粘贴正文，粘贴不了再改为卡片显示
fn complete(
    mode: Mode,
    source: &str,
    output: &str,
    replace: impl FnOnce(&str) -> bool,
) -> Result<Completion> {
    let (body, notes) = if mode.notes { super::split_notes(output) } else { (output, "") };
    let text = clean_output(source, body);
    ensure!(!text.trim().is_empty(), "AI 返回了空内容");
    if mode.replace {
        if text == source {
            return Ok(if mode.notes {
                Completion::Card(Kind::Info, "原文已经很自然，无需修改 👍".into(), 2500)
            } else {
                Completion::Silent
            });
        }
        if replace(&text) {
            return Ok(if notes.is_empty() {
                Completion::Silent
            } else {
                Completion::Card(Kind::Tutor, notes.to_owned(), 0)
            });
        }
    }
    let mut result = text.trim().to_owned();
    if !notes.is_empty() {
        result.push_str("\n\n");
        result.push_str(notes);
    }
    Ok(Completion::Card(Kind::Result, result, 0))
}

pub(super) fn run(
    mode: Mode,
    config: AiSettings,
    owner: usize,
    target: selection::Target,
    cursor: (i32, i32),
) {
    unsafe {
        let _ = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    let fallback = anchor::mouse_rect(cursor);
    let url = endpoint(&config.base_url).and_then(|url| {
        ensure!(!config.model.trim().is_empty(), "请先在 AI 设置中填写模型");
        Ok(url)
    });
    let url = match url {
        Ok(url) => url,
        Err(error) => return show_notice(fallback, Kind::Error, &error.to_string(), 0),
    };

    let fg = HWND(target.foreground() as _);
    let (send, receive) = mpsc::channel();
    let _ = std::thread::Builder::new()
        .name("quick-formater-anchor".into())
        .spawn(move || {
            let rect = anchor::selected_rect();
            if target.is_current() {
                let _ = send.send(rect);
            }
        });
    let range = mode.replace.then(|| target.writable_range()).flatten();
    // 弹窗标题直接用动作名
    let title = config.actions[mode.index].name.trim().to_owned();
    let captured = selection::capture(owner, target);
    let anchor = receive
        .recv_timeout(Duration::from_millis(200))
        .ok()
        .flatten()
        .and_then(anchor::clip_to_monitor)
        .or_else(|| anchor::caret_rect(fg).and_then(anchor::clip_to_monitor))
        .unwrap_or(fallback);
    let source = match captured {
        Ok(source) => Arc::new(source),
        Err(error) => return show_notice(anchor, Kind::Info, &error.to_string(), 2500),
    };

    let tid = message_queue();
    let _hooks = ScopedHooks::install(tid);
    spinner::show(anchor);
    let cancelled = Arc::new(AtomicBool::new(false));
    let worker_cancelled = cancelled.clone();
    let worker_source = source.clone();
    let (send, receive) = mpsc::channel();
    let worker = std::thread::Builder::new()
        .name("quick-formater-ai".into())
        .spawn(move || {
            let post = |event| {
                if !worker_cancelled.load(Ordering::Acquire) && send.send(event).is_ok() {
                    unsafe {
                        let _ = PostThreadMessageW(tid, WM_APP_UPDATE, WPARAM(0), LPARAM(0));
                    }
                }
            };
            let result = request(&config, url, mode, &worker_source, |delta| {
                ensure!(!worker_cancelled.load(Ordering::Acquire), "请求已取消");
                if !mode.replace && !delta.is_empty() {
                    post(Update::Delta(delta.to_owned()));
                }
                Ok(())
            })
            .and_then(|output| {
                ensure!(!worker_cancelled.load(Ordering::Acquire), "请求已取消");
                let active = || !worker_cancelled.load(Ordering::Acquire) && (!mode.replace || range.is_some() && range == target.writable_range());
                complete(
                    mode,
                    &worker_source,
                    &output,
                    |text| {
                        active()
                            && selection::replace(owner, target, &worker_source, text, active)
                                .unwrap_or(false)
                    },
                )
            });
            post(Update::Done(result));
        });

    if let Err(error) = worker {
        spinner::hide();
        card(anchor, Kind::Error, "", &error.to_string(), None, 0);
    }
    let mut pending = true;
    message_loop(|message| match message {
        WM_APP_OUTSIDE if pending && mode.replace => true,
        WM_APP_ESC | WM_APP_OUTSIDE => false,
        WM_APP_UPDATE => {
            let mut delta = String::new();
            let mut completion = None;
            for update in receive.try_iter() {
                match update {
                    Update::Delta(text) => delta.push_str(&text),
                    Update::Done(result) => completion = Some(result),
                }
            }
            // A finished result already includes the deltas from this wake-up.
            if !delta.is_empty() && !matches!(&completion, Some(Ok(_))) {
                if !popup::is_open() {
                    spinner::hide();
                    card(anchor, Kind::Result, &title, "", Some(source.as_str()), 0);
                }
                popup::append(&delta);
            }
            if let Some(result) = completion {
                pending = false;
                spinner::hide();
                match result {
                    Ok(Completion::Silent) => return false,
                    Ok(Completion::Card(_, text, _)) if popup::is_open() => popup::finish(&text),
                    Ok(Completion::Card(kind, text, timeout)) => {
                        card(anchor, kind, &title, &text, None, timeout)
                    }
                    Err(error) if popup::is_open() => popup::fail(&error.to_string()),
                    Err(error) => card(anchor, Kind::Error, "", &error.to_string(), None, 0),
                }
            }
            popup::is_open() || spinner::is_open()
        }
        _ => true,
    });
    cancelled.store(true, Ordering::Release);
    spinner::hide();
    popup::close();
}

fn message_queue() -> u32 {
    unsafe {
        // Create the queue before the HTTP worker can post to it.
        let mut message = MSG::default();
        let _ = PeekMessageW(&mut message, None, 0, 0, PM_NOREMOVE);
        GetCurrentThreadId()
    }
}

struct ScopedHooks;
impl ScopedHooks {
    fn install(tid: u32) -> Self {
        hooks::install(tid);
        Self
    }
}
impl Drop for ScopedHooks {
    fn drop(&mut self) {
        hooks::uninstall();
    }
}

fn show_notice(anchor: RECT, kind: Kind, text: &str, timeout: u32) {
    let _hooks = ScopedHooks::install(message_queue());
    card(anchor, kind, "", text, None, timeout);
    if popup::is_open() {
        message_loop(|message| !matches!(message, WM_APP_ESC | WM_APP_OUTSIDE));
        popup::close();
    }
}

fn message_loop(mut thread_message: impl FnMut(u32) -> bool) {
    unsafe {
        let mut message = MSG::default();
        while GetMessageW(&mut message, None, 0, 0).0 > 0 {
            if message.hwnd.0.is_null() && message.message >= WM_APP {
                if !thread_message(message.message) {
                    break;
                }
            } else {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REPLACE: Mode = Mode { index: 0, replace: true, notes: false };
    const REPLACE_NOTES: Mode = Mode { index: 0, replace: true, notes: true };
    const BUBBLE_NOTES: Mode = Mode { index: 0, replace: false, notes: true };

    #[test]
    fn replace_without_notes_preserves_literal_output_and_falls_back_to_a_card() {
        let output = "正文\n<notes>\n代码";
        let result = complete(REPLACE, "原文", output, |body| {
            assert_eq!(body, output);
            false
        }).unwrap();
        assert!(matches!(result, Completion::Card(Kind::Result, text, _) if text == output));
        assert!(matches!(complete(REPLACE, "原文", output, |_| true).unwrap(), Completion::Silent));
    }

    #[test]
    fn notes_are_split_off_and_only_the_body_is_pasted() {
        let result = complete(
            REPLACE_NOTES,
            "  he go",
            "He goes\r\n<notes>\r\n主语是第三人称单数。",
            |text| {
                assert_eq!(text, "  He goes");
                true
            },
        )
        .unwrap();
        assert!(matches!(result, Completion::Card(Kind::Tutor, notes, 0) if notes == "主语是第三人称单数。"));

        let no_paste = complete(REPLACE_NOTES, "helo", "Hello\n<notes>\n拼写", |_| false).unwrap();
        assert!(matches!(no_paste, Completion::Card(Kind::Result, text, 0) if text == "Hello\n\n拼写"));

        let bubble = complete(BUBBLE_NOTES, "x", "译文\n<notes>\n说明", |_| panic!("bubble never pastes")).unwrap();
        assert!(matches!(bubble, Completion::Card(Kind::Result, text, 0) if text == "译文\n\n说明"));
    }

    #[test]
    fn unchanged_text_is_silent_unless_notes_are_on() {
        let silent = complete(REPLACE, "Hello", "Hello", |_| panic!("unchanged text should not be pasted")).unwrap();
        assert!(matches!(silent, Completion::Silent));
        let info = complete(REPLACE_NOTES, "Hello", "Hello", |_| panic!("unchanged text should not be pasted")).unwrap();
        assert!(matches!(info, Completion::Card(Kind::Info, _, 2500)));
    }
}
