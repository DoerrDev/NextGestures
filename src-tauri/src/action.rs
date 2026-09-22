use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result};
use gesture_core::Action;
use gesture_platform::{clipboard, input, shell, window};

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 动作可能阻塞（起进程 / 跑 lua），单独开线程执行
pub fn spawn(action: Action) {
    std::thread::spawn(move || {
        if let Err(e) = run(&action) {
            log::error!("执行动作失败: {e:#}");
        }
    });
}

pub fn run(action: &Action) -> Result<()> {
    match action {
        Action::None => Ok(()),
        Action::Command { program, args, shell } => run_command(program, args, *shell),
        Action::Hotkey { keys } => input::send_hotkey(keys),
        Action::KeySequence { steps } => {
            for step in steps {
                input::send_hotkey(step)?;
                std::thread::sleep(Duration::from_millis(30));
            }
            Ok(())
        }
        Action::OpenUrl { url } => open_url(url),
        Action::Lua { code } => run_lua(code),
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

/// mlua::Error 不是 Send/Sync，转成字符串再交给 anyhow
fn run_lua(code: &str) -> Result<()> {
    lua_exec(code).map_err(|e| anyhow::anyhow!("lua 脚本执行出错: {e}"))
}

fn lua_exec(code: &str) -> mlua::Result<()> {
    let lua = mlua::Lua::new();
    let ng = lua.create_table()?;

    ng.set(
        "send_keys",
        lua.create_function(|_, combo: String| {
            let keys: Vec<String> =
                combo.split('+').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
            input::send_hotkey(&keys).map_err(mlua::Error::external)
        })?,
    )?;

    ng.set(
        "run",
        lua.create_function(|_, (program, args): (String, Option<Vec<String>>)| {
            run_command(&program, &args.unwrap_or_default(), false)
                .map_err(mlua::Error::external)
        })?,
    )?;

    ng.set(
        "selection",
        lua.create_function(|_, ()| Ok(clipboard::read_selection().unwrap_or_default()))?,
    )?;

    ng.set(
        "url_encode",
        lua.create_function(|_, text: String| Ok(percent_encode(text.trim())))?,
    )?;

    ng.set("cursor", lua.create_function(|_, ()| Ok(input::cursor_pos()))?)?;
    ng.set("exe", lua.create_function(|_, ()| Ok(window::foreground_exe()))?)?;
    ng.set(
        "sleep",
        lua.create_function(|_, ms: u64| {
            std::thread::sleep(Duration::from_millis(ms.min(10_000)));
            Ok(())
        })?,
    )?;

    lua.globals().set("ng", ng)?;
    lua.load(code).exec()
}
