//! Air / 轻盈白：原生 UI 的共享设计令牌，修改时同步根目录 design.md。
use windows::Win32::Foundation::COLORREF;

const fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF(r as u32 | (g as u32) << 8 | (b as u32) << 16)
}

pub const SURFACE: COLORREF = rgb(255, 255, 255);
pub const BORDER: COLORREF = rgb(232, 233, 236);
pub const TEXT: COLORREF = rgb(29, 29, 31);
pub const MUTED: COLORREF = rgb(118, 118, 125);
pub const ACCENT: COLORREF = rgb(0, 113, 227);
pub const ACCENT_HOVER: COLORREF = rgb(0, 102, 204);
pub const ACCENT_WASH: COLORREF = rgb(234, 243, 255);
pub const NEUTRAL_HOVER: COLORREF = rgb(245, 245, 247);
pub const ERROR: COLORREF = rgb(207, 62, 62);
pub const SCROLL: COLORREF = rgb(196, 201, 209);
pub const SPINNER_ACCENT: [f32; 3] = [0.0, 113.0 / 255.0, 227.0 / 255.0];

// 以下为 96 DPI 下的逻辑像素，绘制前按所在显示器缩放。
pub const PAD: f32 = 20.0;
pub const HEADER_H: f32 = 52.0;
pub const FOOTER_GAP: f32 = 16.0;
pub const BUTTON_H: f32 = 30.0;
pub const BOTTOM_PAD: f32 = 16.0;
pub const MIN_TEXT_W: f32 = 280.0;
pub const MAX_TEXT_W: f32 = 370.0;
pub const ANCHOR_GAP: f32 = 8.0;
pub const WINDOW_RADIUS: f32 = 17.0;
pub const SHADOW_PAD: f32 = 16.0;
