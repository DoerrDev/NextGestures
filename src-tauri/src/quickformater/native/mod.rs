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

fn card(anchor: RECT, kind: Kind, text: &str, estimate: Option<&str>, auto_close_ms: u32) {
    popup::open(
        anchor,
        Options {
            kind,
            text,
            estimate,
            streaming: estimate.is_some(),
            font_size: 10.5,
            auto_close_ms,
        },
    );
}

fn complete(
    mode: Mode,
    show_polish_notes: bool,
    source: &str,
    output: &str,
    replace: impl FnOnce(&str) -> bool,
) -> Result<Completion> {
    let kind = if mode == Mode::Jyutping {
        Kind::Yueping
    } else {
        Kind::Translation
    };
    if mode != Mode::Polish {
        let text = clean_output(source, output);
        ensure!(!text.trim().is_empty(), "AI 返回了空内容");
        return Ok(Completion::Card(kind, text, 0));
    }

    let (body, notes) = super::split_polish_notes(output);
    let notes = if show_polish_notes { notes } else { "" };
    let text = clean_output(source, body);
    ensure!(!text.trim().is_empty(), "AI 返回了空内容");
    if text == source {
        return Ok(if show_polish_notes {
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
                if mode != Mode::Polish && !delta.is_empty() {
                    post(Update::Delta(delta.to_owned()));
                }
                Ok(())
            })
            .and_then(|output| {
                ensure!(!worker_cancelled.load(Ordering::Acquire), "请求已取消");
                let active = || !worker_cancelled.load(Ordering::Acquire);
                complete(
                    mode,
                    config.show_polish_notes,
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

    let kind = if mode == Mode::Jyutping {
        Kind::Yueping
    } else {
        Kind::Translation
    };
    if let Err(error) = worker {
        spinner::hide();
        card(anchor, Kind::Error, &error.to_string(), None, 0);
    }
    message_loop(|message| match message {
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
                    card(anchor, kind, "", Some(source.as_str()), 0);
                }
                popup::append(&delta);
            }
            if let Some(result) = completion {
                spinner::hide();
                match result {
                    Ok(Completion::Silent) => return false,
                    Ok(Completion::Card(_, text, _)) if popup::is_open() => popup::finish(&text),
                    Ok(Completion::Card(kind, text, timeout)) => {
                        card(anchor, kind, &text, None, timeout)
                    }
                    Err(error) if popup::is_open() => popup::fail(&error.to_string()),
                    Err(error) => card(anchor, Kind::Error, &error.to_string(), None, 0),
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
    card(anchor, kind, text, None, timeout);
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

    #[test]
    fn polish_pastes_only_corrected_text_with_either_notes_setting() {
        for show_notes in [true, false] {
            let result = complete(
                Mode::Polish,
                show_notes,
                "  he go",
                "He goes\r\n<notes>\r\n主语是第三人称单数。",
                |text| {
                    assert_eq!(text, "  He goes");
                    true
                },
            )
            .unwrap();
            if show_notes {
                assert!(
                    matches!(result, Completion::Card(Kind::Tutor, notes, 0) if notes == "主语是第三人称单数。")
                );
            } else {
                assert!(matches!(result, Completion::Silent));
            }
        }
    }

    #[test]
    fn polish_without_notes_keeps_copyable_fallback_but_skips_no_change_popup() {
        let unchanged = complete(Mode::Polish, false, "Hello", "Hello", |_| {
            panic!("unchanged text should not be pasted")
        })
        .unwrap();
        assert!(matches!(unchanged, Completion::Silent));

        let no_paste = complete(
            Mode::Polish,
            false,
            "helo",
            "Hello\n<notes>\n拼写",
            |_| false,
        )
        .unwrap();
        assert!(matches!(no_paste, Completion::Card(Kind::Result, text, 0) if text == "Hello"));
    }
}
