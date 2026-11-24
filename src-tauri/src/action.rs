use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result};
use gesture_core::Action;
use gesture_platform::{clipboard, input, shell};
use tauri::{AppHandle, Manager};

use crate::state::AppState;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 动作可能阻塞（起进程 / 取选中文字），单独开线程执行
pub fn spawn(app: AppHandle, action: Action) {
    std::thread::spawn(move || {
        if let Err(e) = run(&app, &action) {
            log::error!("执行动作失败: {e:#}");
            // The AI worker handles errors after it starts. This covers launch and URL errors.
            if action.is_ai() {
                crate::engine::toast(&app, &e.to_string());
            }
            if matches!(action, Action::LaunchApp { .. } | Action::OpenUrl { .. }) {
                crate::engine::toast(&app, &e.to_string());
            }
        }
    });
}

pub fn run(app: &AppHandle, action: &Action) -> Result<()> {
    match action {
        Action::None => Ok(()),
        Action::Screenshot
        | Action::RecordRegion
        | Action::AiJyutping
        | Action::AiTranslate
        | Action::AiPolish => {
            let plugins = app.state::<AppState>().config.read().unwrap().plugins.clone();
            crate::plugins::execute(app, action, &plugins)
        }
        Action::Command { program, args, shell } => run_command(program, args, *shell),
        Action::LaunchApp { app_id, name } => {
            shell::open_installed_app(app_id).with_context(|| format!("启动失败: {name}"))
        }
        Action::Hotkey { keys } => input::send_hotkey(keys),
        Action::KeySequence { steps, events } => {
            if !events.is_empty() {
                return input::replay_sequence(events);
            }
            for (index, step) in steps.iter().enumerate() {
                if index > 0 {
                    std::thread::sleep(Duration::from_millis(30));
                }
                input::send_hotkey(step)?;
            }
            Ok(())
        }
        Action::OpenUrl { url } => open_url(url),
    }
}

const SELECTION: &str = "{selection}";

fn open_url(template: &str) -> Result<()> {
    let url = if template.contains(SELECTION) {
        let text = clipboard::read_selection().unwrap_or_default();
        template.replace(SELECTION, &percent_encode(text.trim()))
    } else {
        template.to_string()
    };
    shell::open_url(&url)
}

/// URL 查询串编码，未保留字符原样保留，其余按 UTF-8 逐字节转义
fn percent_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for b in text.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn run_command(program: &str, args: &[String], shell: bool) -> Result<()> {
    let mut cmd = if shell {
        let mut c = Command::new("cmd");
        c.arg("/C").arg(program).args(args);
        c
    } else {
        let mut c = Command::new(program);
        c.args(args);
        c
    };
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd.spawn().with_context(|| format!("启动失败: {program}"))?;
    Ok(())
}
