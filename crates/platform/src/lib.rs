#[cfg(not(windows))]
compile_error!("目前仅支持 Windows，移植时在此处新增 platform 模块实现");

#[cfg(windows)]
mod windows;

#[cfg(windows)]
pub use windows::{clipboard, hook, hotkey, input, shell, window};

/// 鼠标状态快照
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MouseState {
    pub x: i32,
    pub y: i32,
    pub left: bool,
    pub right: bool,
    pub middle: bool,
    pub x1: bool,
    pub x2: bool,
    /// 累计滚轮刻度，向上为正
    pub wheel: i32,
}
