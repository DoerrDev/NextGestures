//! 贴图的独立工具栏窗口：lucide 图标按钮 + 悬浮提示，选中贴图时显示。
//! 图标、色块和按钮底色都用涂鸦光栅器抗锯齿绘制后整块贴到按钮上。
use anyhow::{Result, ensure};
use std::cell::RefCell;
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{COLORREF, HANDLE, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::Controls::{
    DRAWITEMSTRUCT, ICC_WIN95_CLASSES, INITCOMMONCONTROLSEX, InitCommonControlsEx, ODS_DISABLED,
    ODS_FOCUS, ODS_SELECTED, TOOLTIPS_CLASSW, TTF_IDISHWND, TTF_SUBCLASS, TTM_ADDTOOLW,
    TTS_ALWAYSTIP, TTS_NOPREFIX, TTTOOLINFOW,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, PWSTR, w};

use super::pin_icons as icons;

pub(super) const COMMAND: u32 = WM_APP + 43;
/// 贴图、工具栏或文字框的激活状态变化后投递给贴图，重新判断是否处于选中状态。
pub(super) const SELECTION: u32 = WM_APP + 44;
pub(super) const COPY: u16 = 101;
pub(super) const SAVE: u16 = 102;
pub(super) const LESS: u16 = 103;
pub(super) const MORE: u16 = 104;
pub(super) const CLOSE: u16 = 105;
pub(super) const RECT_TOOL: u16 = 111;
pub(super) const LINE_TOOL: u16 = 112;
pub(super) const PEN: u16 = 113;
pub(super) const MARKER: u16 = 114;
pub(super) const MOSAIC: u16 = 115;
pub(super) const TEXT: u16 = 116;
pub(super) const ERASER: u16 = 117;
pub(super) const UNDO: u16 = 118;
pub(super) const PALETTE: u16 = 120;
pub(super) const SWATCH: u16 = 121;
/// 默认选色板（COLORREF 0x00BBGGRR）：红、橙、黄、绿、蓝、紫、黑、白。
pub(super) const SWATCHES: [u32; 8] = [
    0x2828e6, 0x008cff, 0x00d6ff, 0x46b428, 0xff781e, 0xdc4696, 0x000000, 0xffffff,
];
const SWATCH_NAMES: [&str; 8] = ["红色", "橙色", "黄色", "绿色", "蓝色", "紫色", "黑色", "白色"];
const ALPHA_SLOT: u16 = 0;
const SPACER: u16 = 1;
/// (id, 宽度, 行)
const ITEMS: [(u16, i32, i32); 25] = [
    (RECT_TOOL, 30, 0),
    (LINE_TOOL, 30, 0),
    (PEN, 30, 0),
    (MARKER, 30, 0),
    (MOSAIC, 30, 0),
    (TEXT, 30, 0),
    (ERASER, 30, 0),
    (SPACER, 8, 0),
    (UNDO, 30, 0),
    (COPY, 30, 0),
    (SAVE, 30, 0),
    (CLOSE, 30, 0),
    (SWATCH, 26, 1),
    (SWATCH + 1, 26, 1),
    (SWATCH + 2, 26, 1),
    (SWATCH + 3, 26, 1),
    (SWATCH + 4, 26, 1),
    (SWATCH + 5, 26, 1),
    (SWATCH + 6, 26, 1),
    (SWATCH + 7, 26, 1),
    (PALETTE, 30, 1),
    (SPACER, 8, 1),
    (LESS, 26, 1),
    (ALPHA_SLOT, 42, 1),
    (MORE, 26, 1),
];
const GAP: i32 = 2;
const WIDTH: i32 = 384;
const HEIGHT: i32 = 74;

/// 未缩放的按钮位置 (id, x, y, 宽)。
fn slots() -> impl Iterator<Item = (u16, i32, i32, i32)> {
    let mut x = [8, 8];
    ITEMS.into_iter().map(move |(id, width, row)| {
        let left = x[row as usize];
        x[row as usize] += width + GAP;
        (id, left, 5 + row * 34, width)
    })
}

fn tip(id: u16) -> String {
    match id {
        RECT_TOOL => "矩形 / 椭圆  Ctrl+1（再按切换，Shift 画正形）".into(),
        LINE_TOOL => "箭头 / 直线  Ctrl+2（按住拖动，再按切换，Shift 锁定 45°）".into(),
        PEN => "画笔  Ctrl+3".into(),
        MARKER => "记号笔  Ctrl+4".into(),
        MOSAIC => "马赛克  Ctrl+5".into(),
        TEXT => "文字  Ctrl+6（Enter 确定，Shift+Enter 换行）".into(),
        ERASER => "橡皮擦  E".into(),
        UNDO => "撤销  Ctrl+Z".into(),
        COPY => "复制图片  Ctrl+C".into(),
        SAVE => "保存图片  Ctrl+S".into(),
        CLOSE => "关闭贴图  Esc".into(),
        PALETTE => "调色板（自定义颜色）".into(),
        LESS => "降低不透明度（滚轮也可调节）".into(),
        MORE => "提高不透明度".into(),
        id if (SWATCH..SWATCH + 8).contains(&id) => SWATCH_NAMES[(id - SWATCH) as usize].into(),
        _ => String::new(),
    }
}

fn icon(id: u16, alt: bool) -> Option<&'static str> {
    Some(match id {
        RECT_TOOL if alt => icons::CIRCLE,
        RECT_TOOL => icons::SQUARE,
        LINE_TOOL if alt => icons::SLASH,
        LINE_TOOL => icons::ARROW,
        PEN => icons::PENCIL,
        MARKER => icons::HIGHLIGHTER,
        MOSAIC => icons::GRID,
        TEXT => icons::TYPE,
        ERASER => icons::ERASER,
        UNDO => icons::UNDO,
        COPY => icons::COPY,
        SAVE => icons::SAVE,
        CLOSE => icons::X,
        PALETTE => icons::PALETTE,
        LESS => icons::MINUS,
        MORE => icons::PLUS,
        _ => return None,
    })
}

struct Toolbar {
    window: HWND,
    owner: HWND,
    buttons: Vec<(u16, HWND)>,
    font: HFONT,
    scale: f64,
    alpha: u8,
    feedback: Option<(u16, Instant)>,
    busy: bool,
    selected: bool,
    tool: u16,
    /// 工具的另一形态：矩形→椭圆、箭头→直线。
    alt: bool,
    color: COLORREF,
    hot: u16,
}
impl Drop for Toolbar {
    fn drop(&mut self) {
        if !self.font.is_invalid() {
            unsafe {
                let _ = DeleteObject(HGDIOBJ(self.font.0));
            }
        }
    }
}
thread_local! { static BAR: RefCell<Option<Toolbar>> = const { RefCell::new(None) }; }

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}
fn color(r: u8, g: u8, b: u8) -> COLORREF {
    COLORREF(u32::from(r) | (u32::from(g) << 8) | (u32::from(b) << 16))
}
fn scaled(value: i32, scale: f64) -> i32 {
    (f64::from(value) * scale).round() as i32
}

fn toolbar_region(width: i32, height: i32, scale: f64) -> HRGN {
    let diameter = scaled(16, scale).max(2);
    unsafe { CreateRoundRectRgn(0, 0, width, height, diameter, diameter) }
}

const FRAME_PROPERTY: PCWSTR = w!("NextGesturesToolbarFrame");

fn toolbar_frame(hwnd: HWND) -> HWND {
    unsafe { HWND(GetPropW(hwnd, FRAME_PROPERTY).0) }
}

pub fn set_toolbar_rounding(hwnd: HWND, width: i32, height: i32, scale: f64, exclude: bool) {
    if width <= 0 || height <= 0 {
        return;
    }
    unsafe {
        if update_toolbar_frame(hwnd, width, height, scale, exclude).is_ok() {
            return;
        }
        if let Ok(frame) = RemovePropW(hwnd, FRAME_PROPERTY) {
            let _ = DestroyWindow(HWND(frame.0));
        }
    }
    apply_toolbar_region(hwnd, toolbar_region(width, height, scale));
}

fn apply_toolbar_region(hwnd: HWND, region: HRGN) -> bool {
    if !region.is_invalid() {
        unsafe {
            if SetWindowRgn(hwnd, Some(region), true) != 0 {
                return true;
            }
            let _ = DeleteObject(region.into());
        }
    }
    false
}

fn update_toolbar_frame(
    hwnd: HWND,
    width: i32,
    height: i32,
    scale: f64,
    exclude: bool,
) -> Result<()> {
    let mut bounds = RECT::default();
    unsafe { GetWindowRect(hwnd, &mut bounds)? };
    let mut surface = super::pin::Surface::new(width, height)?;
    let radius = (8.0 * scale as f32).max(2.0);
    let stroke = (scale as f32).max(1.0);
    let inset = scaled(2, scale).max(2);
    let hole = (inset + scaled(3, scale).max(2)) as f32;
    let (w, h) = (width as f32, height as f32);
    for (index, pixel) in surface.bytes_mut().chunks_exact_mut(4).enumerate() {
        let x = (index % width as usize) as f32 + 0.5;
        let y = (index / width as usize) as f32 + 0.5;
        let coverage = |margin: f32| {
            let hw = w / 2.0 - margin;
            let hh = h / 2.0 - margin;
            let r = (radius - margin).max(0.0).min(hw).min(hh);
            let qx = (x - w / 2.0).abs() - (hw - r);
            let qy = (y - h / 2.0).abs() - (hh - r);
            let distance = qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - r;
            (0.5 - distance).clamp(0.0, 1.0)
        };
        let outer = coverage(0.0);
        let inner = coverage(stroke).min(outer);
        let cutout = coverage(hole).min(inner);
        let rim = outer - inner;
        let white = (inner - cutout) * 255.0;
        for (channel, ink) in pixel[..3].iter_mut().zip([207.0, 198.0, 190.0]) {
            *channel = (white + ink * rim).round() as u8;
        }
        pixel[3] = ((outer - cutout) * 255.0).round() as u8;
    }
    unsafe {
        let mut frame = toolbar_frame(hwnd);
        if frame.is_invalid() {
            // This owned, click-through window supplies only the antialiased rim.
            // The original window still paints and handles its native controls.
            frame = CreateWindowExW(
                WS_EX_LAYERED
                    | WS_EX_TRANSPARENT
                    | WS_EX_NOACTIVATE
                    | WS_EX_TOOLWINDOW
                    | WS_EX_TOPMOST,
                w!("STATIC"),
                w!(""),
                WS_POPUP,
                bounds.left,
                bounds.top,
                width,
                height,
                Some(hwnd),
                None,
                None,
                None,
            )?;
            if let Err(error) = SetPropW(hwnd, FRAME_PROPERTY, Some(HANDLE(frame.0))) {
                let _ = DestroyWindow(frame);
                return Err(error.into());
            }
            if exclude {
                SetWindowDisplayAffinity(frame, WDA_EXCLUDEFROMCAPTURE)?;
            }
        }
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
            ..Default::default()
        };
        UpdateLayeredWindow(
            frame,
            None,
            Some(&POINT {
                x: bounds.left,
                y: bounds.top,
            }),
            Some(&SIZE {
                cx: width,
                cy: height,
            }),
            Some(surface.dc),
            Some(&POINT::default()),
            COLORREF(0),
            Some(&blend),
            ULW_ALPHA,
        )?;
        // Leave the host's binary clip underneath the opaque white part of the rim.
        let diameter = ((radius - inset as f32) * 2.0).round().max(2.0) as i32;
        let region = CreateRoundRectRgn(
            inset,
            inset,
            width - inset,
            height - inset,
            diameter,
            diameter,
        );
        ensure!(apply_toolbar_region(hwnd, region), "设置工具栏内容边缘失败");
        show_toolbar_frame(hwnd, IsWindowVisible(hwnd).as_bool());
    }
    Ok(())
}

pub fn show_toolbar_frame(hwnd: HWND, visible: bool) {
    let frame = toolbar_frame(hwnd);
    if !frame.is_invalid() {
        unsafe {
            let _ = ShowWindow(frame, if visible { SW_SHOWNOACTIVATE } else { SW_HIDE });
        }
    }
}

pub fn release_toolbar_frame(hwnd: HWND) {
    unsafe {
        // The owned HWND is destroyed with its owner.
        let _ = RemovePropW(hwnd, FRAME_PROPERTY);
    }
}

pub fn paint_toolbar_surface(hwnd: HWND, dc: HDC, rect: &RECT, scale: f64) {
    unsafe {
        let _ = FillRect(dc, rect, HBRUSH(GetStockObject(WHITE_BRUSH).0));
        if !toolbar_frame(hwnd).is_invalid() {
            return;
        }
        let region = toolbar_region(rect.right, rect.bottom, scale);
        if region.is_invalid() {
            return;
        }
        let brush = CreateSolidBrush(color(190, 198, 207));
        if !brush.is_invalid() {
            // Stroke inside the same rounded region that clips the window.
            let thickness = scaled(1, scale).max(1);
            let _ = FrameRgn(dc, region, brush, thickness, thickness);
            let _ = DeleteObject(brush.into());
        }
        let _ = DeleteObject(region.into());
    }
}

/// 按钮内容的离屏 BGRA 缓冲；所有图形都按覆盖率混合，边缘抗锯齿。
struct Pixels {
    w: i32,
    h: i32,
    data: Vec<u8>,
}
impl Pixels {
    fn new(w: i32, h: i32, background: COLORREF) -> Self {
        let c = background.0;
        let px = [(c >> 16) as u8, (c >> 8) as u8, c as u8, 255];
        Self {
            w,
            h,
            data: px.repeat((w.max(0) * h.max(0)) as usize),
        }
    }
    fn blend(&mut self, i: usize, cov: f32, color: COLORREF) {
        if cov <= 0.0 {
            return;
        }
        let cov = cov.min(1.0);
        let c = color.0;
        for (k, v) in [(c >> 16) as u8, (c >> 8) as u8, c as u8].into_iter().enumerate() {
            let d = &mut self.data[i * 4 + k];
            *d = (f32::from(*d) + (f32::from(v) - f32::from(*d)) * cov).round() as u8;
        }
    }
    /// 以有符号距离场（内部为负）填充形状。
    fn fill(&mut self, color: COLORREF, distance: impl Fn(f32, f32) -> f32) {
        for y in 0..self.h {
            for x in 0..self.w {
                let d = distance(x as f32 + 0.5, y as f32 + 0.5);
                self.blend((y * self.w + x) as usize, 0.5 - d, color);
            }
        }
    }
    fn disc(&mut self, cx: f32, cy: f32, r: f32, color: COLORREF) {
        self.fill(color, |x, y| ((x - cx).powi(2) + (y - cy).powi(2)).sqrt() - r);
    }
    fn ring(&mut self, cx: f32, cy: f32, r: f32, width: f32, color: COLORREF) {
        self.fill(color, |x, y| {
            (((x - cx).powi(2) + (y - cy).powi(2)).sqrt() - r).abs() - width / 2.0
        });
    }
    fn round_rect(&mut self, inset: f32, radius: f32, color: COLORREF) {
        let (hw, hh) = (self.w as f32 / 2.0 - inset, self.h as f32 / 2.0 - inset);
        let (cx, cy) = (self.w as f32 / 2.0, self.h as f32 / 2.0);
        self.fill(color, |x, y| {
            let qx = (x - cx).abs() - (hw - radius);
            let qy = (y - cy).abs() - (hh - radius);
            (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt() + qx.max(qy).min(0.0) - radius
        });
    }
    fn icon(&mut self, svg: &str, size: f32, cx: f32, cy: f32, color: COLORREF) {
        let k = size / 24.0;
        let (ox, oy) = (cx - size / 2.0, cy - size / 2.0);
        let paths: Vec<_> = icons::outline(svg)
            .into_iter()
            .map(|(points, closed)| {
                (
                    points
                        .into_iter()
                        .map(|(x, y)| (ox + x * k, oy + y * k))
                        .collect(),
                    closed,
                )
            })
            .collect();
        let cov = super::pin_draw::stroke_coverage(&paths, (2.0 * k).max(1.2), self.w, self.h);
        for (i, c) in cov.into_iter().enumerate() {
            if c > 0 {
                self.blend(i, f32::from(c) / 255.0, color);
            }
        }
    }
    fn blit(&self, dc: HDC, x: i32, y: i32) {
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: self.w,
                biHeight: -self.h,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        unsafe {
            SetDIBitsToDevice(
                dc,
                x,
                y,
                self.w as u32,
                self.h as u32,
                0,
                0,
                0,
                self.h as u32,
                self.data.as_ptr().cast(),
                &info,
                DIB_RGB_COLORS,
            );
        }
    }
}

/// 下方空间不足时翻到图片上方；小贴图也有完整工具栏，限制在所在屏工作区内。
fn placement(image: RECT, work: RECT, dpi: u32) -> (RECT, f64) {
    let scale = (f64::from(dpi.max(96)) / 96.0)
        .min(f64::from((work.right - work.left - 16).max(1)) / f64::from(WIDTH));
    let width = scaled(WIDTH, scale).max(1);
    let height = scaled(HEIGHT, scale).max(1);
    let left = (image.left + (image.right - image.left - width) / 2)
        .clamp(work.left, (work.right - width).max(work.left));
    let below = image.bottom + scaled(8, scale);
    let top = if below + height <= work.bottom {
        below
    } else {
        image.top - scaled(8, scale) - height
    };
    let top = top.clamp(work.top, (work.bottom - height).max(work.top));
    (
        RECT {
            left,
            top,
            right: left + width,
            bottom: top + height,
        },
        scale,
    )
}

fn layout(owner: HWND) -> Result<(RECT, f64)> {
    let mut image = RECT::default();
    unsafe {
        GetWindowRect(owner, &mut image)?;
    }
    image.left += super::pin::SHADOW;
    image.top += super::pin::SHADOW;
    image.right -= super::pin::SHADOW;
    image.bottom -= super::pin::SHADOW;
    let monitor = unsafe {
        MonitorFromPoint(
            POINT {
                x: (image.left + image.right) / 2,
                y: (image.top + image.bottom) / 2,
            },
            MONITOR_DEFAULTTONEAREST,
        )
    };
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    ensure!(
        unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool(),
        "读取工具栏显示器失败"
    );
    Ok(placement(image, info.rcWork, unsafe {
        GetDpiForWindow(owner)
    }))
}

pub(super) fn create(owner: HWND, alpha: u8) -> Result<()> {
    static CLASS: OnceLock<bool> = OnceLock::new();
    ensure!(
        *CLASS.get_or_init(|| unsafe {
            let _ = InitCommonControlsEx(&INITCOMMONCONTROLSEX {
                dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
                dwICC: ICC_WIN95_CLASSES,
            });
            RegisterClassW(&WNDCLASSW {
                lpfnWndProc: Some(procedure),
                lpszClassName: w!("NextGesturesPinToolbar"),
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                ..Default::default()
            }) != 0
        }),
        "注册贴图工具栏失败"
    );
    let (rect, scale) = layout(owner)?;
    let window = unsafe {
        CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_CONTROLPARENT,
            w!("NextGesturesPinToolbar"),
            w!("贴图工具栏"),
            WS_POPUP | WS_CLIPCHILDREN,
            rect.left,
            rect.top,
            rect.right - rect.left,
            rect.bottom - rect.top,
            Some(owner),
            None,
            None,
            None,
        )?
    };
    let result = (|| -> Result<Toolbar> {
        let mut buttons = Vec::new();
        for (id, x, y, width) in slots() {
            if id == ALPHA_SLOT || id == SPACER {
                continue;
            }
            let button = unsafe {
                CreateWindowExW(
                    WINDOW_EX_STYLE(0),
                    w!("BUTTON"),
                    w!(""),
                    WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_OWNERDRAW as u32),
                    scaled(x, scale),
                    scaled(y, scale),
                    scaled(width, scale),
                    scaled(30, scale),
                    Some(window),
                    Some(HMENU(id as usize as *mut _)),
                    None,
                    None,
                )?
            };
            buttons.push((id, button));
        }
        // 悬浮提示：工具名称 + 快捷键。
        unsafe {
            let tooltip = CreateWindowExW(
                WS_EX_TOPMOST,
                TOOLTIPS_CLASSW,
                None,
                WS_POPUP | WINDOW_STYLE(TTS_ALWAYSTIP | TTS_NOPREFIX),
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                Some(window),
                None,
                None,
                None,
            )?;
            for &(id, button) in &buttons {
                let mut text = wide(&tip(id));
                let info = TTTOOLINFOW {
                    // V2 大小同时兼容 comctl32 v5/v6。
                    cbSize: (size_of::<TTTOOLINFOW>() - size_of::<usize>()) as u32,
                    uFlags: TTF_IDISHWND | TTF_SUBCLASS,
                    hwnd: window,
                    uId: button.0 as usize,
                    lpszText: PWSTR(text.as_mut_ptr()),
                    ..Default::default()
                };
                SendMessageW(
                    tooltip,
                    TTM_ADDTOOLW,
                    None,
                    Some(LPARAM(&info as *const _ as isize)),
                );
            }
        }
        Ok(Toolbar {
            window,
            owner,
            buttons,
            font: HFONT::default(),
            scale,
            alpha,
            feedback: None,
            busy: false,
            selected: false,
            tool: 0,
            alt: false,
            color: COLORREF(SWATCHES[0]),
            hot: 0,
        })
    })();
    match result {
        Ok(state) => BAR.with(|bar| *bar.borrow_mut() = Some(state)),
        Err(e) => {
            unsafe {
                let _ = DestroyWindow(window);
            }
            return Err(e);
        }
    }
    position()?;
    Ok(())
}

pub(super) fn position() -> Result<()> {
    let owner = BAR.with(|b| b.borrow().as_ref().map(|b| b.owner));
    let Some(owner) = owner else { return Ok(()) };
    let (rect, scale) = layout(owner)?;
    let state = BAR.with(|b| {
        let mut b = b.borrow_mut();
        let b = b.as_mut().unwrap();
        if b.font.is_invalid() || (b.scale - scale).abs() > 0.01 {
            let mut font = LOGFONTW {
                lfHeight: -scaled(12, scale),
                ..Default::default()
            };
            let face: Vec<u16> = "Microsoft YaHei UI".encode_utf16().collect();
            font.lfFaceName[..face.len()].copy_from_slice(&face);
            let next = unsafe { CreateFontIndirectW(&font) };
            if !next.is_invalid() {
                let old = std::mem::replace(&mut b.font, next);
                if !old.is_invalid() {
                    unsafe {
                        let _ = DeleteObject(HGDIOBJ(old.0));
                    }
                }
            }
        }
        b.scale = scale;
        (b.window, b.buttons.clone())
    });
    unsafe {
        SetWindowPos(
            state.0,
            Some(HWND_TOPMOST),
            rect.left,
            rect.top,
            rect.right - rect.left,
            rect.bottom - rect.top,
            SWP_NOACTIVATE,
        )?;
        set_toolbar_rounding(state.0, rect.right - rect.left, rect.bottom - rect.top, scale, false);
        for (id, x, y, width) in slots() {
            if let Some((_, button)) = state.1.iter().find(|(key, _)| *key == id) {
                SetWindowPos(
                    *button,
                    None,
                    scaled(x, scale),
                    scaled(y, scale),
                    scaled(width, scale),
                    scaled(30, scale),
                    SWP_NOZORDER | SWP_NOACTIVATE,
                )?;
                let _ = InvalidateRect(Some(*button), None, true);
            }
        }
        let _ = InvalidateRect(Some(state.0), None, true);
    }
    Ok(())
}

pub(super) fn set_selected(selected: bool) {
    let changed = BAR.with(|b| {
        let mut b = b.borrow_mut();
        b.as_mut().is_some_and(|b| {
            std::mem::replace(&mut b.selected, selected) != selected
        })
    });
    if changed {
        apply_visibility();
    }
}

/// 选中的贴图显示工具栏，打开模态对话框时暂时隐藏。
fn apply_visibility() {
    let state = BAR.with(|b| b.borrow().as_ref().map(|b| (b.window, b.selected && !b.busy)));
    let Some((window, visible)) = state else { return };
    unsafe {
        if visible {
            // 每次显示都按贴图当前位置 / 大小重新定位（如展开折叠后）。
            if let Err(e) = position() {
                log::warn!("贴图工具栏定位失败: {e}");
            }
            if !IsWindowVisible(window).as_bool() {
                let _ = ShowWindow(window, SW_SHOWNOACTIVATE);
                SetTimer(Some(window), 1, 100, None);
            }
        } else {
            let _ = ShowWindow(window, SW_HIDE);
            let _ = KillTimer(Some(window), 1);
        }
    }
}

pub(super) fn alpha(value: u8) {
    let window = BAR.with(|b| {
        let mut b = b.borrow_mut();
        b.as_mut().map(|b| {
            b.alpha = value;
            b.window
        })
    });
    if let Some(window) = window {
        unsafe {
            let _ = InvalidateRect(Some(window), None, true);
        }
    }
}

/// 同步当前工具（按钮 id，0 = 无）、工具形态（椭圆 / 直线）和颜色到工具栏显示。
pub(super) fn sync(tool: u16, alt: bool, color: COLORREF) {
    let buttons = BAR.with(|b| {
        let mut b = b.borrow_mut();
        b.as_mut().map(|b| {
            b.tool = tool;
            b.alt = alt;
            b.color = color;
            b.buttons.clone()
        })
    });
    for (_, button) in buttons.unwrap_or_default() {
        unsafe {
            let _ = InvalidateRect(Some(button), None, false);
        }
    }
}

pub(super) fn translate(message: &MSG) -> bool {
    let window = BAR.with(|b| b.borrow().as_ref().map(|b| b.window));
    window.is_some_and(|window| unsafe {
        IsWindowVisible(window).as_bool() && IsDialogMessageW(window, message).as_bool()
    })
}

pub(super) fn busy(value: bool) {
    BAR.with(|b| {
        if let Some(b) = b.borrow_mut().as_mut() {
            b.busy = value;
        }
    });
    apply_visibility();
}

/// 复制 / 保存成功后按钮图标短暂变为对勾。
pub(super) fn feedback(id: u16) {
    let button = BAR.with(|b| {
        let mut b = b.borrow_mut();
        b.as_mut().and_then(|b| {
            b.feedback = Some((id, Instant::now() + Duration::from_secs(2)));
            b.buttons
                .iter()
                .find(|(key, _)| *key == id)
                .map(|(_, h)| *h)
        })
    });
    if let Some(button) = button {
        unsafe {
            let _ = InvalidateRect(Some(button), None, false);
        }
    }
}

pub(super) fn error(message: &str) {
    let window = BAR.with(|b| b.borrow().as_ref().map(|b| b.window));
    if let Some(window) = window {
        let text = wide(message);
        unsafe {
            MessageBoxW(
                Some(window),
                PCWSTR(text.as_ptr()),
                w!("贴图操作失败"),
                MB_OK | MB_ICONERROR,
            );
        }
    }
}

/// 定时器：维护悬浮高亮并复位复制 / 保存的对勾反馈。
fn tick() {
    let mut cursor = POINT::default();
    let under = if unsafe { GetCursorPos(&mut cursor) }.is_ok() {
        unsafe { WindowFromPoint(cursor) }
    } else {
        HWND::default()
    };
    let repaint = BAR.with(|b| {
        let mut b = b.borrow_mut();
        let b = b.as_mut()?;
        let mut repaint = Vec::new();
        let hot = b
            .buttons
            .iter()
            .find(|(_, h)| *h == under)
            .map_or(0, |(id, _)| *id);
        if hot != b.hot {
            repaint.extend([b.hot, hot]);
            b.hot = hot;
        }
        if let Some((id, until)) = b.feedback {
            if Instant::now() >= until {
                b.feedback = None;
                repaint.push(id);
            }
        }
        Some(
            b.buttons
                .iter()
                .filter(|(id, _)| repaint.contains(id))
                .map(|(_, h)| *h)
                .collect::<Vec<_>>(),
        )
    });
    for button in repaint.unwrap_or_default() {
        unsafe {
            let _ = InvalidateRect(Some(button), None, false);
        }
    }
}

fn draw_item(item: &DRAWITEMSTRUCT) {
    let id = item.CtlID as u16;
    let pressed = item.itemState.0 & ODS_SELECTED.0 != 0;
    let Some((tool, alt, current, scale, hot, feedback)) = BAR.with(|b| {
        b.borrow().as_ref().map(|b| {
            (
                b.tool,
                b.alt,
                b.color,
                b.scale as f32,
                b.hot,
                b.feedback.map(|(id, _)| id),
            )
        })
    }) else {
        return;
    };
    let rc = item.rcItem;
    let (w, h) = (rc.right - rc.left, rc.bottom - rc.top);
    let active = tool != 0 && id == tool;
    let swatch = (SWATCH..SWATCH + SWATCHES.len() as u16).contains(&id);
    let mut px = button_surface(
        w,
        h,
        scale,
        !swatch && pressed,
        !swatch && active,
        !swatch && hot == id,
    );
    let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
    if swatch {
        let value = COLORREF(SWATCHES[(id - SWATCH) as usize]);
        let r = 7.0 * scale + if hot == id { 0.8 * scale } else { 0.0 };
        if value == current {
            px.ring(cx, cy, r + 3.0 * scale, 1.6 * scale, color(37, 99, 235));
        }
        px.disc(cx, cy, r, value);
        if value.0 == 0xffffff {
            px.ring(cx, cy, r - 0.5, 1.0, color(190, 198, 207));
        }
    } else if let Some(svg) = icon(id, alt) {
        let (svg, ink) = if feedback == Some(id) {
            (icons::CHECK, color(22, 150, 80))
        } else if id == CLOSE {
            (svg, color(200, 60, 60))
        } else if active {
            (svg, color(29, 100, 210))
        } else {
            (svg, color(51, 65, 85))
        };
        px.icon(svg, 18.0 * scale, cx, cy, ink);
        if id == PALETTE {
            // 右下角显示当前颜色。
            let (dx, dy, r) = (w as f32 - 8.0 * scale, h as f32 - 8.0 * scale, 4.0 * scale);
            px.disc(dx, dy, r + 1.5 * scale, color(255, 255, 255));
            px.disc(dx, dy, r, current);
            px.ring(dx, dy, r, 1.0, color(150, 160, 170));
        }
    }
    px.blit(item.hDC, rc.left, rc.top);
}

fn button_surface(w: i32, h: i32, scale: f32, pressed: bool, active: bool, hot: bool) -> Pixels {
    let mut pixels = Pixels::new(w, h, color(255, 255, 255));
    if pressed || active || hot {
        let fill = if pressed {
            color(203, 222, 245)
        } else if active {
            color(219, 234, 254)
        } else {
            color(238, 242, 247)
        };
        pixels.round_rect(1.0, 6.0 * scale, fill);
    }
    pixels
}

/// Recorder buttons share the screenshot toolbar's colors and Lucide renderer.
#[derive(Clone, Copy)]
pub enum RecordingButton {
    Start,
    Stop,
    Settings,
    Close,
}

pub fn draw_recording_button(
    item: &DRAWITEMSTRUCT,
    scale: f32,
    kind: RecordingButton,
    label: &str,
    hot: bool,
) {
    let rect = item.rcItem;
    let (width, height) = (rect.right - rect.left, rect.bottom - rect.top);
    let disabled = item.itemState.0 & ODS_DISABLED.0 != 0;
    let pressed = item.itemState.0 & ODS_SELECTED.0 != 0;
    let focus = item.itemState.0 & ODS_FOCUS.0 != 0;
    let mut pixels = button_surface(
        width,
        height,
        scale,
        pressed,
        false,
        !disabled && (hot || focus),
    );
    let (svg, destructive) = match kind {
        RecordingButton::Start => (include_str!("../../assets/lucide/circle-dot.svg"), true),
        RecordingButton::Stop => (icons::SQUARE, true),
        RecordingButton::Settings => (include_str!("../../assets/lucide/settings-2.svg"), false),
        RecordingButton::Close => (icons::X, true),
    };
    let ink = if disabled {
        color(148, 163, 184)
    } else if destructive {
        color(200, 60, 60)
    } else {
        color(51, 65, 85)
    };
    let icon_x = if label.is_empty() {
        width as f32 / 2.0
    } else {
        16.0 * scale
    };
    pixels.icon(svg, 18.0 * scale, icon_x, height as f32 / 2.0, ink);
    pixels.blit(item.hDC, rect.left, rect.top);
    if !label.is_empty() {
        let mut text = label.encode_utf16().collect::<Vec<_>>();
        let mut area = RECT {
            left: rect.left + (30.0 * scale).round() as i32,
            right: rect.right - (5.0 * scale).round() as i32,
            ..rect
        };
        unsafe {
            let font = GetStockObject(DEFAULT_GUI_FONT);
            let previous = SelectObject(item.hDC, font);
            SetBkMode(item.hDC, TRANSPARENT);
            SetTextColor(item.hDC, if disabled { ink } else { color(51, 65, 85) });
            DrawTextW(
                item.hDC,
                &mut text,
                &mut area,
                DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS | DT_NOPREFIX,
            );
            SelectObject(item.hDC, previous);
        }
    }
}

unsafe extern "system" fn procedure(hwnd: HWND, message: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match message {
            WM_COMMAND => {
                let id = (wp.0 & 0xffff) as u16;
                let id = if i32::from(id) == IDCANCEL.0 {
                    CLOSE
                } else {
                    id
                };
                let owner = BAR.with(|b| b.borrow().as_ref().filter(|b| !b.busy).map(|b| b.owner));
                if let Some(owner) = owner {
                    let _ = PostMessageW(Some(owner), COMMAND, WPARAM(id as usize), LPARAM(0));
                }
                LRESULT(0)
            }
            WM_ACTIVATE => {
                let owner = BAR.with(|b| b.borrow().as_ref().map(|b| b.owner));
                if let Some(owner) = owner {
                    let _ = PostMessageW(Some(owner), SELECTION, WPARAM(0), LPARAM(0));
                }
                DefWindowProcW(hwnd, message, wp, lp)
            }
            WM_TIMER => {
                tick();
                LRESULT(0)
            }
            WM_ERASEBKGND => LRESULT(1),
            WM_SHOWWINDOW => {
                show_toolbar_frame(hwnd, wp.0 != 0);
                DefWindowProcW(hwnd, message, wp, lp)
            }
            WM_PAINT => {
                let mut paint = PAINTSTRUCT::default();
                let dc = BeginPaint(hwnd, &mut paint);
                let mut rect = RECT::default();
                let _ = GetClientRect(hwnd, &mut rect);
                let style = BAR.with(|b| b.borrow().as_ref().map(|b| (b.scale, b.font, b.alpha)));
                paint_toolbar_surface(hwnd, dc, &rect, style.map_or(1.0, |(scale, _, _)| scale));
                if let Some((scale, font, alpha)) = style {
                    let previous = SelectObject(dc, HGDIOBJ(font.0));
                    SetBkMode(dc, TRANSPARENT);
                    SetTextColor(dc, color(71, 85, 105));
                    let mut text: Vec<u16> = format!("{}%", (u32::from(alpha) * 100 + 127) / 255)
                        .encode_utf16()
                        .collect();
                    let (_, x, y, width) = slots().find(|slot| slot.0 == ALPHA_SLOT).unwrap();
                    let mut area = RECT {
                        left: scaled(x, scale),
                        top: scaled(y, scale),
                        right: scaled(x + width, scale),
                        bottom: scaled(y + 30, scale),
                    };
                    DrawTextW(
                        dc,
                        &mut text,
                        &mut area,
                        DT_CENTER | DT_VCENTER | DT_SINGLELINE,
                    );
                    // 两行之间的细分隔线。
                    let line = RECT {
                        left: scaled(8, scale),
                        top: scaled(37, scale),
                        right: rect.right - scaled(8, scale),
                        bottom: scaled(37, scale) + 1,
                    };
                    let brush = CreateSolidBrush(color(232, 236, 241));
                    FillRect(dc, &line, brush);
                    let _ = DeleteObject(HGDIOBJ(brush.0));
                    SelectObject(dc, previous);
                }
                let _ = EndPaint(hwnd, &paint);
                LRESULT(0)
            }
            WM_DRAWITEM => {
                draw_item(&*(lp.0 as *const DRAWITEMSTRUCT));
                LRESULT(1)
            }
            WM_CLOSE => {
                let owner = BAR.with(|b| b.borrow().as_ref().map(|b| b.owner));
                if let Some(owner) = owner {
                    let _ = PostMessageW(Some(owner), WM_CLOSE, WPARAM(0), LPARAM(0));
                }
                LRESULT(0)
            }
            WM_DESTROY => {
                release_toolbar_frame(hwnd);
                BAR.with(|b| {
                    b.borrow_mut().take();
                });
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, message, wp, lp),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn toolbar_flips_above_taskbar_and_stays_on_negative_coordinate_monitor() {
        let work = RECT {
            left: -1920,
            top: 0,
            right: 0,
            bottom: 1040,
        };
        let image = RECT {
            left: -200,
            top: 900,
            right: -20,
            bottom: 1030,
        };
        let (bar, _) = placement(image, work, 144);
        assert!(bar.left >= work.left && bar.right <= work.right);
        assert!(bar.bottom < image.top && bar.top >= work.top);
        let (bar, _) = placement(
            RECT {
                left: -1000,
                top: 100,
                right: -936,
                bottom: 164,
            },
            work,
            192,
        );
        assert!(bar.top > 164 && bar.right - bar.left == WIDTH * 2);
    }
    #[test]
    #[ignore = "writes an icon preview PNG to NEXTGESTURES_ICON_PREVIEW"]
    fn icon_preview() {
        let Ok(path) = std::env::var("NEXTGESTURES_ICON_PREVIEW") else { return };
        let ids = [
            RECT_TOOL, LINE_TOOL, PEN, MARKER, MOSAIC, TEXT, ERASER, UNDO, COPY, SAVE, CLOSE,
            PALETTE, LESS, MORE,
        ];
        let mut rows = Vec::new();
        for scale in [1.0f32, 1.5, 2.0] {
            let size = (30.0 * scale) as i32;
            let mut row = Pixels::new(size * (ids.len() as i32 + 9), size, color(255, 255, 255));
            let mut put = |slot: i32, px: &Pixels| {
                for y in 0..size {
                    let dst = ((y * row.w + slot * size) * 4) as usize;
                    let src = (y * size * 4) as usize;
                    row.data[dst..dst + (size * 4) as usize]
                        .copy_from_slice(&px.data[src..src + (size * 4) as usize]);
                }
            };
            for (n, id) in ids.iter().enumerate() {
                let mut px = Pixels::new(size, size, color(255, 255, 255));
                if n == 2 {
                    px.round_rect(1.0, 6.0 * scale, color(219, 234, 254));
                }
                px.icon(icon(*id, false).unwrap(), 18.0 * scale, size as f32 / 2.0, size as f32 / 2.0, color(51, 65, 85));
                put(n as i32, &px);
            }
            for (n, value) in SWATCHES.iter().enumerate() {
                let mut px = Pixels::new(size, size, color(255, 255, 255));
                let c = size as f32 / 2.0;
                if n == 0 {
                    px.ring(c, c, 10.0 * scale, 1.6 * scale, color(37, 99, 235));
                }
                px.disc(c, c, 7.0 * scale, COLORREF(*value));
                put(ids.len() as i32 + n as i32, &px);
            }
            rows.push(row);
        }
        let width = rows.iter().map(|r| r.w).max().unwrap();
        let height: i32 = rows.iter().map(|r| r.h).sum();
        let mut rgba = vec![255u8; (width * height * 4) as usize];
        let mut top = 0;
        for row in &rows {
            for y in 0..row.h {
                for x in 0..row.w {
                    let s = ((y * row.w + x) * 4) as usize;
                    let d = (((top + y) * width + x) * 4) as usize;
                    rgba[d] = row.data[s + 2];
                    rgba[d + 1] = row.data[s + 1];
                    rgba[d + 2] = row.data[s];
                }
            }
            top += row.h;
        }
        let file = std::fs::File::create(path).unwrap();
        let mut encoder = png::Encoder::new(file, width as u32, height as u32);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.write_header().unwrap().write_image_data(&rgba).unwrap();
    }
    #[test]
    fn both_rows_fit_inside_the_toolbar_width() {
        let right = slots().map(|(_, x, _, w)| x + w).max().unwrap();
        assert!(right + 8 <= WIDTH, "row ends at {right}");
    }
    #[test]
    fn swatch_disc_edges_are_anti_aliased() {
        let mut px = Pixels::new(20, 20, color(255, 255, 255));
        px.disc(10.0, 10.0, 6.0, color(0, 0, 0));
        let gray = px.data.chunks_exact(4).filter(|p| p[0] > 20 && p[0] < 235).count();
        assert!(gray >= 8, "expected soft edge pixels, got {gray}");
        assert_eq!(&px.data[(10 * 20 + 10) * 4..][..3], &[0, 0, 0]);
    }
}
