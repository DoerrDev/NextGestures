//! 物理像素截图与逐像素阴影；每张贴图独立拥有图片及消息循环。
//!
//! 类 Snipaste 的两个阶段：
//! - 截图：在冻结的整屏画面上框选，选区可调整（开始标注后锁定），工具栏常驻，
//!   退出 / 贴图 / 保存 / 复制分别对应 Esc / Ctrl+T / Ctrl+S / Ctrl+C。
//! - 贴图：置顶的图片，可拖动、滚轮缩放；空格打开工具栏编辑，Esc 完成；
//!   单按 Ctrl 折叠成缩略图；无工具栏时 Esc 关闭并压入历史栈，贴图键可依次恢复。
use anyhow::{Result, bail, ensure};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use windows::Win32::Foundation::{
    COLORREF, HANDLE, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM,
};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForWindow, SetThreadDpiAwarenessContext,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetCapture, GetKeyState, ReleaseCapture, SetCapture, VIRTUAL_KEY, VK_CONTROL, VK_ESCAPE,
    VK_MENU, VK_RETURN, VK_SPACE,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::w;

use super::pin_draw::{self as draw, Canvas, Ink, P, Shape, Tool};
use super::pin_text;
use super::pin_toolbar as bar;
use super::region::{Backdrop, Frozen};

#[path = "pin_ink.rs"]
mod ink;
use ink::with_picture;

pub(super) const SHADOW: i32 = 20;
const FOLD_SIZE: i32 = 64;
/// 单按 Ctrl：折叠成缩略图 / 恢复。
const TOGGLE_FOLD: u16 = 151;
/// 空格：打开 / 收起贴图工具栏。
const TOGGLE_TOOLBAR: u16 = 152;
const ZOOM_STEP: f64 = 1.1;
const MAX_IMAGE_SIDE: i32 = 16384;
const MAX_IMAGE_PIXELS: i32 = 32 * 1024 * 1024;
const MIN_SIDE: i32 = 16;
const ZOOM_BADGE_TIMER: usize = 8;
const FLASH_TIMER: usize = 9;
/// 历史栈已空时投递给每张贴图：半透明 500ms 作为提示。
const FLASH: u32 = WM_APP + 45;
const FLASH_MS: u32 = 500;
/// 关闭贴图的历史栈最多保留的张数。
const HISTORY: usize = 20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bounds {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}
impl Bounds {
    pub fn from_points(points: &[(i32, i32)]) -> Result<Self> {
        let &(mut left, mut top) = points
            .first()
            .ok_or_else(|| anyhow::anyhow!("请画出截图范围"))?;
        let (mut right, mut bottom) = (left, top);
        for &(x, y) in points {
            left = left.min(x);
            right = right.max(x);
            top = top.min(y);
            bottom = bottom.max(y);
        }
        let (width, height) = (
            i64::from(right) - i64::from(left),
            i64::from(bottom) - i64::from(top),
        );
        ensure!(width >= 16 && height >= 16, "截图区域至少为 16 × 16 像素");
        ensure!(
            width <= i64::from(MAX_IMAGE_SIDE)
                && height <= i64::from(MAX_IMAGE_SIDE)
                && width * height <= i64::from(MAX_IMAGE_PIXELS),
            "截图范围过大，请缩小区域"
        );
        Ok(Self {
            x: left,
            y: top,
            width: width as i32,
            height: height as i32,
        })
    }
}

/// top-down BGRA DIB；仅阴影使用半透明像素，符合预乘 alpha 格式。
pub(super) struct Surface {
    pub(super) dc: HDC,
    bitmap: HBITMAP,
    previous: HGDIOBJ,
    pixels: *mut u8,
    width: i32,
    height: i32,
}
impl Surface {
    pub(super) fn new(width: i32, height: i32) -> Result<Self> {
        unsafe {
            let dc = CreateCompatibleDC(None);
            ensure!(!dc.is_invalid(), "创建图片缓冲区失败");
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width,
                    biHeight: -height,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut pixels = std::ptr::null_mut();
            let bitmap =
                match CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut pixels, None, 0) {
                    Ok(bitmap) => bitmap,
                    Err(e) => {
                        let _ = DeleteDC(dc);
                        return Err(e.into());
                    }
                };
            let previous = SelectObject(dc, HGDIOBJ(bitmap.0));
            let surface = Self {
                dc,
                bitmap,
                previous,
                pixels: pixels.cast(),
                width,
                height,
            };
            ensure!(!pixels.is_null(), "图片像素为空");
            Ok(surface)
        }
    }
    pub(super) fn bytes_mut(&mut self) -> &mut [u8] {
        unsafe {
            std::slice::from_raw_parts_mut(
                self.pixels,
                self.width as usize * self.height as usize * 4,
            )
        }
    }
}
impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.previous);
            let _ = DeleteObject(HGDIOBJ(self.bitmap.0));
            let _ = DeleteDC(self.dc);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct View {
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}
struct Picture {
    pixels: Vec<u8>,
    width: i32,
    height: i32,
    view: View,
    folded: bool,
    surface: Surface,
    zoom: f64,
    zoom_surface: Option<Surface>,
    zoom_wheel_remainder: i32,
    alpha: u8,
    wheel_remainder: i32,
    exporting: bool,
    /// 被点击选中（贴图或其工具栏/文字框处于前台）。
    selected: bool,
    ink: ink::Annotate,
    /// 贴图阶段按空格打开了工具栏（编辑中）。
    toolbar: bool,
    /// 历史栈为空的提示：暂时半透明。
    flash: bool,
    /// 截图阶段可调整选区时，在边框上画出 8 个控制点。
    handles: bool,
}

impl Picture {
    fn new(pixels: Vec<u8>, width: i32, height: i32) -> Result<Self> {
        ensure!(
            width > 0 && height > 0 && pixels.len() == width as usize * height as usize * 4,
            "图片像素长度错误"
        );
        ensure!(
            width <= MAX_IMAGE_SIDE
                && height <= MAX_IMAGE_SIDE
                && i64::from(width) * i64::from(height) <= i64::from(MAX_IMAGE_PIXELS),
            "图片过大，无法贴出"
        );
        let view = View {
            x: 0,
            y: 0,
            width,
            height,
        };
        let mut picture = Picture {
            ink: ink::Annotate::new(pixels.clone(), bar::SWATCHES[0]),
            pixels,
            width,
            height,
            view,
            folded: false,
            surface: Surface::new(1, 1)?,
            zoom: 1.0,
            zoom_surface: None,
            zoom_wheel_remainder: 0,
            alpha: 255,
            wheel_remainder: 0,
            exporting: false,
            selected: false,
            toolbar: false,
            flash: false,
            handles: false,
        };
        picture.surface = picture.compose(view)?;
        Ok(picture)
    }

    /// 把当前图片（含草稿、正在编辑的文字和控制点）写入已合成 DIB 的内容区。
    pub(super) fn paint_view(&mut self) {
        self.paint_content();
        if self.handles {
            paint_handles(&mut self.surface, self.ink.scale);
        }
    }

    fn png(&self) -> Result<Vec<u8>> {
        let mut rgba = self.pixels.clone();
        for pixel in rgba.chunks_exact_mut(4) {
            pixel.swap(0, 2);
        }
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, self.width as u32, self.height as u32);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            encoder.set_compression(png::Compression::Fast);
            encoder.write_header()?.write_image_data(&rgba)?;
        }
        Ok(bytes)
    }
    fn compose(&self, view: View) -> Result<Surface> {
        let mut surface = Surface::new(view.width + SHADOW * 2, view.height + SHADOW * 2)?;
        paint_frame(&mut surface, self.selected);
        copy_surface_content(&mut surface, &self.pixels, self.width, view);
        if self.handles {
            paint_handles(&mut surface, self.ink.scale);
        }
        Ok(surface)
    }

    fn display_size(&self) -> (i32, i32) {
        let surface = self.zoom_surface.as_ref().unwrap_or(&self.surface);
        (surface.width - 2 * SHADOW, surface.height - 2 * SHADOW)
    }

    fn refresh_zoom(&mut self) -> Result<()> {
        if let Some(surface) = &mut self.zoom_surface {
            scale_surface(&self.surface, surface)?;
        }
        Ok(())
    }
}

fn copy_surface_content(surface: &mut Surface, pixels: &[u8], width: i32, view: View) {
    let stride = surface.width as usize * 4;
    let bytes = surface.bytes_mut();
    for y in 0..view.height {
        let src = ((view.y + y) as usize * width as usize + view.x as usize) * 4;
        let dst = (y + SHADOW) as usize * stride + SHADOW as usize * 4;
        let row_bytes = view.width as usize * 4;
        bytes[dst..dst + row_bytes].copy_from_slice(&pixels[src..src + row_bytes]);
    }
}

fn paint_frame(surface: &mut Surface, selected: bool) {
    let (width, height) = (surface.width - 2 * SHADOW, surface.height - 2 * SHADOW);
    let stride = surface.width as usize * 4;
    let bytes = surface.bytes_mut();
    // Both side shadows have the same profile between the top and bottom corners.
    let mut side = [0; SHADOW as usize * 4];
    for (x, pixel) in side.chunks_exact_mut(4).enumerate() {
        pixel.copy_from_slice(&edge_pixel(x as i32, SHADOW + 3, width, height, selected));
    }
    for y in 0..height + 2 * SHADOW {
        let row = &mut bytes[y as usize * stride..][..stride];
        let right = (width + SHADOW) as usize * 4;
        if (SHADOW + 3..SHADOW + height).contains(&y) {
            row[..side.len()].copy_from_slice(&side);
            for (target, source) in row[right..].chunks_exact_mut(4).zip(side.rchunks_exact(4)) {
                target.copy_from_slice(source);
            }
        } else {
            let (left, rest) = row.split_at_mut(side.len());
            let (middle, right) = rest.split_at_mut(width as usize * 4);
            for (x, (left, right)) in left.chunks_exact_mut(4).zip(right.rchunks_exact_mut(4)).enumerate() {
                let color = edge_pixel(x as i32, y, width, height, selected);
                left.copy_from_slice(&color);
                right.copy_from_slice(&color);
            }
            if !(SHADOW..SHADOW + height).contains(&y) {
                let color = edge_pixel(SHADOW, y, width, height, selected);
                for pixel in middle.chunks_exact_mut(4) {
                    pixel.copy_from_slice(&color);
                }
            }
        }
    }
}

/// 截图选区的 8 个控制点：白底蓝边的小方块，中心落在图片边缘上。
fn paint_handles(surface: &mut Surface, scale: f32) {
    let (width, height) = (surface.width - 2 * SHADOW, surface.height - 2 * SHADOW);
    let half = (3.5 * scale.max(1.0)).round() as i32;
    let stride = surface.width as usize * 4;
    let (surface_width, surface_height) = (surface.width, surface.height);
    let bytes = surface.bytes_mut();
    let xs = [SHADOW, SHADOW + width / 2, SHADOW + width - 1];
    let ys = [SHADOW, SHADOW + height / 2, SHADOW + height - 1];
    for (row, &cy) in ys.iter().enumerate() {
        for (column, &cx) in xs.iter().enumerate() {
            if row == 1 && column == 1 {
                continue;
            }
            for y in (cy - half).max(0)..=(cy + half).min(surface_height - 1) {
                for x in (cx - half).max(0)..=(cx + half).min(surface_width - 1) {
                    let border = (x - cx).abs() == half || (y - cy).abs() == half;
                    let i = y as usize * stride + x as usize * 4;
                    bytes[i..i + 4].copy_from_slice(if border {
                        &[235, 152, 52, 255]
                    } else {
                        &[255, 255, 255, 255]
                    });
                }
            }
        }
    }
}

fn scale_surface(source: &Surface, target: &mut Surface) -> Result<()> {
    let width = target.width - 2 * SHADOW;
    let height = target.height - 2 * SHADOW;
    unsafe {
        SetStretchBltMode(target.dc, HALFTONE);
        SetBrushOrgEx(target.dc, 0, 0, None).ok()?;
        StretchBlt(
            target.dc,
            SHADOW,
            SHADOW,
            width,
            height,
            Some(source.dc),
            SHADOW,
            SHADOW,
            source.width - 2 * SHADOW,
            source.height - 2 * SHADOW,
            SRCCOPY,
        )
        .ok()?;
        // GDI's resampler does not preserve the alpha channel in 32-bit DIBs.
        GdiFlush().ok()?;
    }
    let stride = target.width as usize * 4;
    for row in target
        .bytes_mut()
        .chunks_exact_mut(stride)
        .skip(SHADOW as usize)
        .take(height as usize)
    {
        let start = SHADOW as usize * 4;
        for pixel in row[start..][..width as usize * 4].chunks_exact_mut(4) {
            pixel[3] = 255;
        }
    }
    Ok(())
}

fn scaled_surface(source: &Surface, size: (i32, i32), selected: bool) -> Result<Option<Surface>> {
    let (width, height) = (size.0 + 2 * SHADOW, size.1 + 2 * SHADOW);
    if (width, height) == (source.width, source.height) {
        return Ok(None);
    }
    let mut target = Surface::new(width, height)?;
    paint_frame(&mut target, selected);
    scale_surface(source, &mut target)?;
    Ok(Some(target))
}

fn shadow_alpha(x: i32, y: i32, width: i32, height: i32) -> u8 {
    let dx = (SHADOW - x).max(x - (SHADOW + width - 1)).max(0) as f32;
    // 阴影向下偏移 3px，在预留的 20px 中平滑衰减至透明。
    let dy = (SHADOW + 3 - y).max(y - (SHADOW + height + 2)).max(0) as f32;
    let distance = (dx * dx + dy * dy).sqrt();
    (95.0 * (-distance * distance / 85.0).exp() * (1.0 - distance / 20.0).max(0.0)).round() as u8
}

/// 激活使用蓝色辉光，失焦使用中性灰白。输出预乘 BGRA。
fn edge_pixel(x: i32, y: i32, width: i32, height: i32, selected: bool) -> [u8; 4] {
    let (strength, [blue, green, red]) = if selected {
        (1.8, [235, 152, 52])
    } else {
        (0.8, [245, 245, 245])
    };
    let dx = (SHADOW - x).max(x - (SHADOW + width - 1)).max(0) as f32;
    let dy = (SHADOW - y).max(y - (SHADOW + height - 1)).max(0) as f32;
    let distance = (dx * dx + dy * dy).sqrt();
    let offset = (distance - 1.0).max(0.0);
    let glow = ((65.0 * (-offset * offset / 18.0).exp() + 45.0 * (-offset * offset / 0.8).exp())
        * ((10.0 - distance) / 9.0).clamp(0.0, 1.0)
        * strength)
        .round()
        .min(255.0) as u32;
    let shadow = u32::from(shadow_alpha(x, y, width, height));
    [
        (blue * glow / 255) as u8,
        (green * glow / 255) as u8,
        (red * glow / 255) as u8,
        (glow + shadow * (255 - glow) / 255) as u8,
    ]
}

/// 截图阶段：冻结画面（调整选区时重新裁剪）和暗色背景窗口。
struct Stage {
    frozen: Rc<Frozen>,
    backdrop: Backdrop,
    preview: Option<RECT>,
}

/// 拖动控制点调整选区：命中码、起点光标和起始选区（桌面坐标）。
#[derive(Clone, Copy)]
struct Resize {
    edge: u32,
    origin: POINT,
    rect: RECT,
}

/// 关闭后压入历史栈的贴图，可被贴图键原样恢复（标注仍可撤销重编辑）。
struct Closed {
    base: Vec<u8>,
    pixels: Vec<u8>,
    shapes: Vec<Shape>,
    width: i32,
    height: i32,
    origin: POINT,
    zoom: f64,
    alpha: u8,
}

thread_local! {
    static PICTURE: RefCell<Option<Picture>> = const { RefCell::new(None) };
    static STAGE: RefCell<Option<Stage>> = const { RefCell::new(None) };
    static DRAG_OFFSET: Cell<Option<POINT>> = const { Cell::new(None) };
    static RESIZE: Cell<Option<Resize>> = const { Cell::new(None) };
    static ZOOM_BADGE: Cell<Option<(HWND, POINT)>> = const { Cell::new(None) };
    /// 按下 Ctrl 后尚未按其他键 / 鼠标，松开时视为单按。
    static CTRL_TAP: Cell<bool> = const { Cell::new(false) };
}
static COUNT: AtomicUsize = AtomicUsize::new(0);
/// 截图阶段的窗口（0 表示没有）；贴图键在截图时直接贴出选区。
static STAGING: AtomicUsize = AtomicUsize::new(0);
/// 上次贴出的剪贴板序号；剪贴板没变时贴图键改为恢复历史。
static LAST_SEQUENCE: AtomicU32 = AtomicU32::new(0);
static CLOSED: Mutex<Vec<Closed>> = Mutex::new(Vec::new());

fn staging() -> bool {
    STAGE.with(|stage| stage.borrow().is_some())
}
struct Slot;
impl Drop for Slot {
    fn drop(&mut self) {
        COUNT.fetch_sub(1, Ordering::Relaxed);
    }
}

/// 仅在触发键按下/滚轮时查询，使贴图的原生交互优先于全局手势。
pub(super) fn is_at(point: (i32, i32)) -> bool {
    if COUNT.load(Ordering::Relaxed) == 0 {
        return false;
    }
    unsafe {
        let window = WindowFromPoint(POINT {
            x: point.0,
            y: point.1,
        });
        // 工具栏按钮和保存对话框都是贴图的所属窗口，沿 owner 链一并放行。
        let hwnd = GetAncestor(window, GA_ROOTOWNER);
        !hwnd.is_invalid() && !GetPropW(hwnd, w!("NextGesturesQuickPin")).is_invalid()
    }
}

fn capture(b: Bounds) -> Result<Picture> {
    let mut raw = Surface::new(b.width, b.height)?;
    unsafe {
        let screen = GetDC(None);
        ensure!(!screen.is_invalid(), "获取桌面失败");
        let result = BitBlt(
            raw.dc,
            0,
            0,
            b.width,
            b.height,
            Some(screen),
            b.x,
            b.y,
            SRCCOPY | CAPTUREBLT,
        );
        ReleaseDC(None, screen);
        result?;
        let _ = GdiFlush();
    }
    let mut pixels = raw.bytes_mut().to_vec();
    for pixel in pixels.chunks_exact_mut(4) {
        pixel[3] = 255;
    }
    Picture::new(pixels, b.width, b.height)
}

fn render(hwnd: HWND, position: Option<POINT>) -> Result<()> {
    // 调用窗口 API 前释放借用，允许系统窗口消息同步重入。
    let (dc, size, alpha) = PICTURE.with(|p| {
        let p = p.borrow();
        let p = p.as_ref().expect("pin picture");
        let surface = p.zoom_surface.as_ref().unwrap_or(&p.surface);
        (
            surface.dc,
            SIZE {
                cx: surface.width,
                cy: surface.height,
            },
            if p.flash { p.alpha / 2 } else { p.alpha },
        )
    });
    unsafe {
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: alpha,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        UpdateLayeredWindow(
            hwnd,
            None,
            position.as_ref().map(|p| p as *const POINT),
            Some(&size),
            Some(dc),
            Some(&POINT { x: 0, y: 0 }),
            COLORREF(0),
            Some(&blend),
            ULW_ALPHA,
        )?;
    }
    Ok(())
}

fn toggle_fold(hwnd: HWND) -> Result<()> {
    stop_drag(hwnd);
    hide_zoom_badge(hwnd);
    let mut rect = RECT::default();
    unsafe {
        GetWindowRect(hwnd, &mut rect)?;
    }
    let position = PICTURE.with(|p| -> Result<POINT> {
        let mut p = p.borrow_mut();
        let p = p.as_mut().expect("pin picture");
        let view = if p.folded {
            View {
                x: 0,
                y: 0,
                width: p.width,
                height: p.height,
            }
        } else {
            let width = p.width.min(FOLD_SIZE);
            let height = p.height.min(FOLD_SIZE);
            View {
                x: (p.width - width) / 2,
                y: (p.height - height) / 2,
                width,
                height,
            }
        };
        let surface = p.compose(view)?;
        let zoom_surface = if p.folded {
            scaled_surface(&surface, zoom_size(p.width, p.height, p.zoom), p.selected)?
        } else {
            None
        };
        let (old_width, old_height) = p.display_size();
        let next = zoom_surface.as_ref().unwrap_or(&surface);
        let (width, height) = (next.width - 2 * SHADOW, next.height - 2 * SHADOW);
        // Use the same integer offset in both directions to preserve position over a fold cycle.
        let shift = |old: i32, new: i32| {
            if p.folded {
                -((new - old) / 2)
            } else {
                (old - new) / 2
            }
        };
        let position = POINT {
            x: rect.left + shift(old_width, width),
            y: rect.top + shift(old_height, height),
        };
        p.surface = surface;
        p.zoom_surface = zoom_surface;
        p.view = view;
        p.folded = !p.folded;
        p.zoom_wheel_remainder = 0;
        Ok(position)
    })?;
    render(hwnd, Some(position))?;
    // 折叠后的小图视为失活；展开后若仍在前台则重新选中，工具栏跟随新位置。
    update_selection(hwnd)
}

fn zoom_size(width: i32, height: i32, zoom: f64) -> (i32, i32) {
    let scale = |side| ((f64::from(side) * zoom + 1e-8).floor() as i32).max(1);
    (scale(width), scale(height))
}

fn zoom_picture(hwnd: HWND, delta: i32, cursor: POINT) -> Result<()> {
    // 截图阶段图片与屏幕一一对应，不缩放。
    if DRAG_OFFSET.get().is_some() || staging() {
        return Ok(());
    }
    let mut rect = RECT::default();
    unsafe { GetWindowRect(hwnd, &mut rect)? };
    let transition = with_picture(|p| {
        if p.folded || p.exporting {
            return Ok::<_, anyhow::Error>(None);
        }
        p.zoom_wheel_remainder += delta;
        let steps = p.zoom_wheel_remainder / WHEEL_DELTA as i32;
        p.zoom_wheel_remainder %= WHEEL_DELTA as i32;
        if steps == 0 {
            return Ok(None);
        }
        let longest = f64::from(p.width.max(p.height));
        let min = (32.0 / longest).clamp(0.125, 1.0);
        let max = (f64::from(MAX_IMAGE_SIDE) / longest)
            .min((f64::from(MAX_IMAGE_PIXELS) / (f64::from(p.width) * f64::from(p.height))).sqrt())
            .min(4.0);
        let zoom = (p.zoom * ZOOM_STEP.powi(steps)).clamp(min, max.max(1.0));
        let size = zoom_size(p.width, p.height, zoom);
        let old_size = p.display_size();
        if size == old_size {
            p.zoom = zoom;
            return Ok(Some(None));
        }
        let scaled = scaled_surface(&p.surface, size, p.selected)?;
        let anchor = |origin: i32, pointer: i32, before: i32, after: i32| {
            let fraction =
                (f64::from(pointer - origin - SHADOW) / f64::from(before)).clamp(0.0, 1.0);
            origin + (fraction * f64::from(before - after)).round() as i32
        };
        let position = POINT {
            x: anchor(rect.left, cursor.x, old_size.0, size.0),
            y: anchor(rect.top, cursor.y, old_size.1, size.1),
        };
        let old_zoom = p.zoom;
        let old_surface = std::mem::replace(&mut p.zoom_surface, scaled);
        p.zoom = zoom;
        Ok(Some(Some((position, old_zoom, old_surface))))
    })?;
    if let Some(resize) = transition {
        if let Some((position, old_zoom, old_surface)) = resize {
            if let Err(error) = render(hwnd, Some(position)) {
                with_picture(|p| {
                    p.zoom = old_zoom;
                    p.zoom_surface = old_surface;
                });
                return Err(error);
            }
        }
        let zoom = with_picture(|p| p.zoom);
        show_zoom_badge(hwnd, (zoom * 100.0).round() as u32)?;
    }
    Ok(())
}

fn show_zoom_badge(owner: HWND, percent: u32) -> Result<()> {
    static CLASS: OnceLock<bool> = OnceLock::new();
    unsafe {
        ensure!(
            *CLASS.get_or_init(|| {
                RegisterClassW(&WNDCLASSW {
                    lpfnWndProc: Some(zoom_badge_procedure),
                    lpszClassName: w!("NextGesturesPinZoom"),
                    ..Default::default()
                }) != 0
            }),
            "注册缩放提示窗口失败"
        );
        let mut cursor = POINT::default();
        GetCursorPos(&mut cursor)?;
        let mut rect = RECT::default();
        GetWindowRect(owner, &mut rect)?;
        let scale = f64::from(GetDpiForWindow(owner).max(96)) / 96.0;
        let dip = |value: i32| (f64::from(value) * scale).round() as i32;
        let text: Vec<_> = format!("{percent}%").encode_utf16().chain([0]).collect();
        let badge = if let Some((badge, _)) = ZOOM_BADGE.get() {
            badge
        } else {
            let badge = CreateWindowExW(
                WS_EX_TOPMOST
                    | WS_EX_TOOLWINDOW
                    | WS_EX_NOACTIVATE
                    | WS_EX_TRANSPARENT
                    | WS_EX_LAYERED,
                w!("NextGesturesPinZoom"),
                None,
                WS_POPUP,
                0,
                0,
                0,
                0,
                Some(owner),
                None,
                None,
                None,
            )?;
            ZOOM_BADGE.set(Some((badge, cursor)));
            let result = SetLayeredWindowAttributes(badge, COLORREF(0), 238, LWA_ALPHA);
            if result.is_err() || SetTimer(Some(owner), ZOOM_BADGE_TIMER, 16, None) == 0 {
                hide_zoom_badge(owner);
                result?;
                anyhow::bail!("启动缩放提示计时器失败");
            }
            badge
        };
        ZOOM_BADGE.set(Some((badge, cursor)));
        let result = (|| {
            SetWindowTextW(badge, windows::core::PCWSTR(text.as_ptr()))?;
            SetWindowPos(
                badge,
                Some(HWND_TOPMOST),
                rect.left + SHADOW + dip(6),
                rect.top + SHADOW + dip(6),
                dip(52),
                dip(26),
                SWP_NOACTIVATE | SWP_SHOWWINDOW,
            )?;
            let _ = InvalidateRect(Some(badge), None, false);
            Ok(())
        })();
        if result.is_err() {
            hide_zoom_badge(owner);
        }
        result
    }
}

fn hide_zoom_badge(owner: HWND) {
    if let Some((badge, _)) = ZOOM_BADGE.take() {
        unsafe {
            let _ = KillTimer(Some(owner), ZOOM_BADGE_TIMER);
            let _ = DestroyWindow(badge);
        }
    }
}

unsafe extern "system" fn zoom_badge_procedure(
    hwnd: HWND,
    message: u32,
    wp: WPARAM,
    lp: LPARAM,
) -> LRESULT {
    unsafe {
        match message {
            WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
            WM_ERASEBKGND => LRESULT(1),
            WM_PAINT => {
                let mut paint = PAINTSTRUCT::default();
                let dc = BeginPaint(hwnd, &mut paint);
                let mut rect = RECT::default();
                let _ = GetClientRect(hwnd, &mut rect);
                let _ = FillRect(dc, &rect, HBRUSH(GetStockObject(DKGRAY_BRUSH).0));
                let size = (14 * GetDpiForWindow(hwnd).max(96) / 96) as i32;
                let font = CreateFontIndirectW(&LOGFONTW {
                    lfHeight: -size,
                    ..Default::default()
                });
                let previous = SelectObject(dc, font.into());
                let _ = SetBkMode(dc, TRANSPARENT);
                let _ = SetTextColor(dc, COLORREF(0x00ffffff));
                let mut text = [0; 12];
                let len = GetWindowTextW(hwnd, &mut text) as usize;
                DrawTextW(
                    dc,
                    &mut text[..len],
                    &mut rect,
                    DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
                );
                let _ = SelectObject(dc, previous);
                let _ = DeleteObject(font.into());
                let _ = EndPaint(hwnd, &paint);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, message, wp, lp),
        }
    }
}

/// 截图阶段尚未标注时选区可调整（类 Snipaste：开始标注后锁定）。
fn adjustable() -> bool {
    staging() && with_picture(|p| !p.ink.marked())
}

/// 选区边缘的命中码（左 / 右 / 上 / 下及四角），不在边缘附近时为 None。
fn edge_at(image: RECT, point: POINT, grip: i32) -> Option<u32> {
    let reach = |value: i32, low: i32, high: i32| (low - grip..=high + grip).contains(&value);
    if !reach(point.x, image.left, image.right) || !reach(point.y, image.top, image.bottom) {
        return None;
    }
    let near = |value: i32, edge: i32| (value - edge).abs() <= grip;
    let (left, right) = (near(point.x, image.left), near(point.x, image.right));
    let (top, bottom) = (near(point.y, image.top), near(point.y, image.bottom));
    Some(match (left, right, top, bottom) {
        (true, _, true, _) => HTTOPLEFT,
        (_, true, true, _) => HTTOPRIGHT,
        (true, _, _, true) => HTBOTTOMLEFT,
        (_, true, _, true) => HTBOTTOMRIGHT,
        (true, ..) => HTLEFT,
        (_, true, ..) => HTRIGHT,
        (_, _, true, _) => HTTOP,
        (_, _, _, true) => HTBOTTOM,
        _ => return None,
    })
}

/// 当前图片在桌面上的矩形（窗口去掉阴影边距）。
fn image_rect(hwnd: HWND) -> Result<RECT> {
    let mut rect = RECT::default();
    unsafe { GetWindowRect(hwnd, &mut rect)? };
    Ok(RECT {
        left: rect.left + SHADOW,
        top: rect.top + SHADOW,
        right: rect.right - SHADOW,
        bottom: rect.bottom - SHADOW,
    })
}

fn hit_test(hwnd: HWND, point: POINT) -> LRESULT {
    let Ok(image) = image_rect(hwnd) else {
        return LRESULT(HTTRANSPARENT as isize);
    };
    let (drawing, scale) = with_picture(|p| (p.ink.tool.is_some() && !p.folded, p.ink.scale));
    let stage = staging();
    let adjustable = stage && adjustable();
    if adjustable
        && let Some(edge) = edge_at(image, point, (5.0 * scale.max(1.0)).round() as i32)
    {
        return LRESULT(edge as isize);
    }
    let (width, height) = with_picture(|p| p.display_size());
    if (image.left..image.left + width).contains(&point.x)
        && (image.top..image.top + height).contains(&point.y)
    {
        // 涂鸦模式走客户区；普通模式用标题区消息区分拖动和双击；
        // 截图阶段标注后选区锁定，不再拖动。
        LRESULT(if drawing || (stage && !adjustable) {
            HTCLIENT
        } else {
            HTCAPTION
        } as isize)
    } else {
        LRESULT(HTTRANSPARENT as isize)
    }
}

/// 截图阶段：更新预览，拖动结束后才同步用于标注和导出的图片。
fn preview_crop(hwnd: HWND, rect: RECT) -> Result<()> {
    let Some(frozen) = STAGE.with(|stage| stage.borrow().as_ref().map(|s| s.frozen.clone())) else {
        return Ok(());
    };
    let screen = frozen.screen;
    let width = (rect.right - rect.left).clamp(MIN_SIDE, screen.right - screen.left);
    let height = (rect.bottom - rect.top).clamp(MIN_SIDE, screen.bottom - screen.top);
    let left = rect.left.clamp(screen.left, screen.right - width);
    let top = rect.top.clamp(screen.top, screen.bottom - height);
    let rect = RECT {
        left,
        top,
        right: left + width,
        bottom: top + height,
    };
    if image_rect(hwnd)? == rect {
        return Ok(());
    }
    with_picture(|p| -> Result<()> {
        if (p.surface.width, p.surface.height) != (width + SHADOW * 2, height + SHADOW * 2) {
            let mut surface = Surface::new(width + SHADOW * 2, height + SHADOW * 2)?;
            paint_frame(&mut surface, p.selected);
            p.surface = surface;
        }
        let stride = p.surface.width as usize * 4;
        let offset = SHADOW as usize * 4;
        for (target, source) in p
            .surface
            .bytes_mut()
            .chunks_exact_mut(stride)
            .skip(SHADOW as usize)
            .zip(frozen.crop_rows(rect))
        {
            target[offset..offset + source.len()].copy_from_slice(source);
        }
        if p.handles {
            paint_handles(&mut p.surface, p.ink.scale);
        }
        Ok(())
    })?;
    render(
        hwnd,
        Some(POINT {
            x: left - SHADOW,
            y: top - SHADOW,
        }),
    )?;
    STAGE.with(|stage| {
        if let Some(stage) = stage.borrow_mut().as_mut() {
            stage.preview = Some(rect);
        }
    });
    // Layered-window updates can change bounds without a position notification.
    bar::position()
}

fn finish_crop() {
    let crop = STAGE.with(|stage| {
        let mut stage = stage.borrow_mut();
        let stage = stage.as_mut()?;
        Some((stage.frozen.clone(), stage.preview.take()?))
    });
    if let Some((frozen, rect)) = crop {
        let pixels = frozen.crop(rect);
        with_picture(|p| {
            p.ink.base.clone_from(&pixels);
            p.pixels = pixels;
            p.width = rect.right - rect.left;
            p.height = rect.bottom - rect.top;
            p.view = View {
                x: 0,
                y: 0,
                width: p.width,
                height: p.height,
            };
        });
    }
}

fn start_resize(hwnd: HWND, edge: u32, cursor: POINT) -> Result<()> {
    let rect = image_rect(hwnd)?;
    unsafe {
        SetCapture(hwnd);
        if GetCapture() == hwnd {
            RESIZE.set(Some(Resize {
                edge,
                origin: cursor,
                rect,
            }));
        }
    }
    Ok(())
}

fn move_resize(hwnd: HWND, resize: Resize) -> Result<()> {
    let mut cursor = POINT::default();
    unsafe { GetCursorPos(&mut cursor)? };
    let (dx, dy) = (cursor.x - resize.origin.x, cursor.y - resize.origin.y);
    let mut rect = resize.rect;
    let edge = resize.edge;
    if matches!(edge, HTLEFT | HTTOPLEFT | HTBOTTOMLEFT) {
        rect.left = (rect.left + dx).min(rect.right - MIN_SIDE);
    }
    if matches!(edge, HTRIGHT | HTTOPRIGHT | HTBOTTOMRIGHT) {
        rect.right = (rect.right + dx).max(rect.left + MIN_SIDE);
    }
    if matches!(edge, HTTOP | HTTOPLEFT | HTTOPRIGHT) {
        rect.top = (rect.top + dy).min(rect.bottom - MIN_SIDE);
    }
    if matches!(edge, HTBOTTOM | HTBOTTOMLEFT | HTBOTTOMRIGHT) {
        rect.bottom = (rect.bottom + dy).max(rect.top + MIN_SIDE);
    }
    // 拖到屏幕外时固定对边，只截断被拖动的一边。
    if let Some(screen) = STAGE.with(|s| s.borrow().as_ref().map(|s| s.frozen.screen)) {
        rect.left = rect.left.max(screen.left);
        rect.top = rect.top.max(screen.top);
        rect.right = rect.right.min(screen.right);
        rect.bottom = rect.bottom.min(screen.bottom);
    }
    preview_crop(hwnd, rect)
}

fn start_drag(hwnd: HWND, cursor: POINT) -> Result<()> {
    let mut rect = RECT::default();
    unsafe {
        GetWindowRect(hwnd, &mut rect)?;
        SetCapture(hwnd);
        if GetCapture() == hwnd {
            DRAG_OFFSET.set(Some(POINT {
                x: cursor.x - rect.left,
                y: cursor.y - rect.top,
            }));
        }
    }
    Ok(())
}

fn move_drag(hwnd: HWND, offset: POINT) -> Result<()> {
    let mut cursor = POINT::default();
    unsafe {
        GetCursorPos(&mut cursor)?;
        // 截图阶段移动的是选区：同尺寸重新裁剪冻结画面。
        if staging() {
            let image = image_rect(hwnd)?;
            let left = cursor.x - offset.x + SHADOW;
            let top = cursor.y - offset.y + SHADOW;
            return preview_crop(
                hwnd,
                RECT {
                    left,
                    top,
                    right: left + image.right - image.left,
                    bottom: top + image.bottom - image.top,
                },
            );
        }
        // 直接使用桌面坐标，避免系统移动循环在松手时把窗口拉回工作区。
        SetWindowPos(
            hwnd,
            None,
            cursor.x - offset.x,
            cursor.y - offset.y,
            0,
            0,
            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        )?;
    }
    Ok(())
}

fn stop_drag(hwnd: HWND) -> bool {
    let dragging = DRAG_OFFSET.take().is_some() | RESIZE.take().is_some();
    if dragging {
        finish_crop();
    }
    unsafe {
        if dragging && GetCapture() == hwnd {
            let _ = ReleaseCapture();
        }
    }
    dragging
}

/// 直接把桌面上的矩形截成贴图（不经框选）。应在独立工作线程调用。
pub fn open(bounds: Bounds) -> Result<()> {
    unsafe {
        SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    let picture = capture(bounds)?;
    run(
        picture,
        POINT {
            x: bounds.x,
            y: bounds.y,
        },
        None,
    )
}

/// 截图：冻结整屏后框选，进入截图编辑；截好整屏画面后调用 `captured`。
/// 阻塞到截图结束（或转成的贴图关闭）。应在独立工作线程调用。
pub fn screenshot(captured: impl FnOnce()) -> Result<()> {
    let selected = super::region::select_frozen(captured).map_err(anyhow::Error::msg)?;
    let Some(((x, y, width, height), frozen, backdrop)) = selected else {
        return Ok(());
    };
    let rect = RECT {
        left: x,
        top: y,
        right: x + width,
        bottom: y + height,
    };
    let mut picture = Picture::new(frozen.crop(rect), width, height)?;
    picture.handles = true;
    run(
        picture,
        POINT { x, y },
        Some(Stage {
            frozen,
            backdrop,
            preview: None,
        }),
    )
}

/// 贴图键：截图中贴出选区；剪贴板有新内容时贴出（图片，或文字排版成的图片）；
/// 否则依次恢复最近关闭的贴图；历史也空了就让所有贴图闪一下半透明。
/// 贴出新图时阻塞到它关闭，应在独立工作线程调用。
pub fn paste() -> Result<()> {
    let stage = STAGING.load(Ordering::Acquire);
    if stage != 0 {
        unsafe {
            PostMessageW(
                Some(HWND(stage as *mut _)),
                bar::COMMAND,
                WPARAM(usize::from(bar::PIN)),
                LPARAM(0),
            )?;
        }
        return Ok(());
    }
    unsafe {
        SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    let sequence = super::clipboard::sequence_number();
    if LAST_SEQUENCE.swap(sequence, Ordering::AcqRel) != sequence {
        match clipboard_picture() {
            Ok(Some(picture)) => {
                let origin = near_cursor(picture.width, picture.height);
                return run(picture, origin, None);
            }
            Ok(None) => {}
            Err(e) => log::warn!("读取剪贴板贴图失败: {e:#}"),
        }
    }
    let closed = CLOSED.lock().ok().and_then(|mut stack| stack.pop());
    if let Some(closed) = closed {
        return restore(closed);
    }
    if flash_all() == 0 {
        bail!("剪贴板里没有图片或文字，也没有可恢复的贴图");
    }
    Ok(())
}

fn clipboard_picture() -> Result<Option<Picture>> {
    use super::clipboard::PinContent;
    let Some(content) = super::clipboard::pin_content()? else {
        return Ok(None);
    };
    let (width, height, pixels) = match content {
        PinContent::Image {
            width,
            height,
            pixels,
        } => (width, height, pixels),
        PinContent::Text { text, rtf } => super::pin_rich::render(&text, rtf.as_deref())?,
    };
    Picture::new(pixels, width, height).map(Some)
}

/// 以光标为中心放置，并尽量留在光标所在显示器的工作区内。
fn near_cursor(width: i32, height: i32) -> POINT {
    let mut cursor = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut cursor);
        let mut info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let monitor = MonitorFromPoint(cursor, MONITOR_DEFAULTTONEAREST);
        let place = |center: i32, size: i32, low: i32, high: i32| {
            (center - size / 2).clamp(low, (high - size).max(low))
        };
        if GetMonitorInfoW(monitor, &mut info).as_bool() {
            let work = info.rcWork;
            return POINT {
                x: place(cursor.x, width, work.left, work.right),
                y: place(cursor.y, height, work.top, work.bottom),
            };
        }
    }
    POINT {
        x: cursor.x - width / 2,
        y: cursor.y - height / 2,
    }
}

/// 恢复历史栈里的贴图：原位置、缩放、透明度，标注仍可继续编辑。
fn restore(closed: Closed) -> Result<()> {
    let mut picture = Picture::new(closed.base, closed.width, closed.height)?;
    picture.ink.set_shapes(closed.shapes);
    picture.pixels = closed.pixels;
    picture.alpha = closed.alpha;
    picture.surface = picture.compose(picture.view)?;
    if (closed.zoom - 1.0).abs() > f64::EPSILON {
        picture.zoom = closed.zoom;
        picture.zoom_surface = scaled_surface(
            &picture.surface,
            zoom_size(closed.width, closed.height, closed.zoom),
            false,
        )?;
    }
    run(picture, closed.origin, None)
}

/// 关闭的贴图压入历史栈（超过上限时丢弃最早的）。
fn push_closed(hwnd: HWND) {
    let Ok(image) = image_rect(hwnd) else { return };
    let closed = PICTURE.with(|p| {
        let p = p.borrow();
        let p = p.as_ref()?;
        // 折叠时窗口只显示中间一块，按展开后的尺寸换算回原位置。
        let (full_width, full_height) = zoom_size(p.width, p.height, p.zoom);
        let (shown_width, shown_height) = p.display_size();
        Some(Closed {
            base: p.ink.base.clone(),
            pixels: p.pixels.clone(),
            shapes: p.ink.shapes(),
            width: p.width,
            height: p.height,
            origin: POINT {
                x: image.left - (full_width - shown_width) / 2,
                y: image.top - (full_height - shown_height) / 2,
            },
            zoom: p.zoom,
            alpha: p.alpha,
        })
    });
    if let (Some(closed), Ok(mut stack)) = (closed, CLOSED.lock()) {
        stack.push(closed);
        if stack.len() > HISTORY {
            stack.remove(0);
        }
    }
}

/// 让本进程所有贴图闪一下半透明；返回贴图数量。
fn flash_all() -> usize {
    use windows::Win32::System::Threading::GetCurrentProcessId;
    let mut count = 0;
    let mut after = None;
    unsafe {
        while let Ok(hwnd) = FindWindowExW(None, after, w!("NextGesturesQuickPin"), None) {
            after = Some(hwnd);
            let mut process = 0;
            GetWindowThreadProcessId(hwnd, Some(&mut process));
            if process == GetCurrentProcessId()
                && !GetPropW(hwnd, w!("NextGesturesQuickPin")).is_invalid()
                && PostMessageW(Some(hwnd), FLASH, WPARAM(0), LPARAM(0)).is_ok()
            {
                count += 1;
            }
        }
    }
    count
}

pub(super) fn activate_native_window(hwnd: HWND) {
    ink::activate(hwnd);
}

/// 创建贴图窗口（图片左上角位于 `origin`）并运行消息循环，直到窗口关闭。
/// 带 `stage` 时为截图阶段。
fn run(picture: Picture, origin: POINT, stage: Option<Stage>) -> Result<()> {
    ensure!(
        COUNT
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| (n < 20)
                .then_some(n + 1))
            .is_ok(),
        "已有 20 张贴图，请先关闭部分贴图"
    );
    let _slot = Slot;
    unsafe {
        SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    static CLASS: OnceLock<bool> = OnceLock::new();
    ensure!(
        *CLASS.get_or_init(|| unsafe {
            RegisterClassW(&WNDCLASSW {
                lpfnWndProc: Some(procedure),
                lpszClassName: w!("NextGesturesQuickPin"),
                style: CS_DBLCLKS,
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                ..Default::default()
            }) != 0
        }),
        "注册贴图窗口失败"
    );
    let (width, height) = picture.display_size();
    let alpha = picture.alpha;
    let staged = stage.is_some();
    PICTURE.with(|p| *p.borrow_mut() = Some(picture));
    STAGE.with(|s| *s.borrow_mut() = stage);
    let result = (|| -> Result<()> {
        unsafe {
            let hwnd = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED,
                w!("NextGesturesQuickPin"),
                w!("贴图 · 拖动移动 / 滚轮缩放 / 空格编辑 / 单按 Ctrl 折叠 / Esc 关闭"),
                WS_POPUP,
                origin.x - SHADOW,
                origin.y - SHADOW,
                width + SHADOW * 2,
                height + SHADOW * 2,
                None,
                None,
                None,
                None,
            )?;
            if let Err(e) = (|| -> Result<()> {
                SetPropW(
                    hwnd,
                    w!("NextGesturesQuickPin"),
                    Some(HANDLE(1usize as *mut _)),
                )?;
                let scale = GetDpiForWindow(hwnd).max(96) as f32 / 96.0;
                with_picture(|p| p.ink.scale = scale);
                if staged {
                    // 控制点按 DPI 重画。
                    with_picture(|p| -> Result<()> {
                        p.surface = p.compose(p.view)?;
                        Ok(())
                    })?;
                }
                render(
                    hwnd,
                    Some(POINT {
                        x: origin.x - SHADOW,
                        y: origin.y - SHADOW,
                    }),
                )?;
                bar::create(hwnd, alpha, staged)?;
                Ok(())
            })() {
                let _ = DestroyWindow(hwnd);
                return Err(e);
            }
            let _ = ShowWindow(hwnd, SW_SHOW);
            if staged {
                STAGE.with(|s| {
                    if let Some(stage) = s.borrow().as_ref() {
                        stage.backdrop.attach(hwnd);
                    }
                });
                STAGING.store(hwnd.0 as usize, Ordering::Release);
            }
            // 新贴图即选中（辉光变亮）；截图阶段同时显示工具栏。
            ink::activate(hwnd);
            let _ = PostMessageW(Some(hwnd), bar::SELECTION, WPARAM(0), LPARAM(0));
            let mut msg = MSG::default();
            loop {
                let status = GetMessageW(&mut msg, None, 0, 0).0;
                if status == -1 {
                    let _ = DestroyWindow(hwnd);
                    anyhow::bail!("贴图消息循环出错");
                }
                if status == 0 {
                    break;
                }
                // 文字编辑中的按键 / 字符（含输入法）优先。
                if ink::pump(hwnd, &msg) {
                    continue;
                }
                if ctrl_tapped(&msg) {
                    command(hwnd, TOGGLE_FOLD);
                    continue;
                }
                // 贴图或其工具栏获得焦点时的快捷键（类似 Snipaste）。
                if let Some(id) = ink::shortcut(&msg) {
                    // WM_KEYDOWN 的第 30 位表示按键此前已按下，长按空格时只切换一次。
                    if id != TOGGLE_TOOLBAR || msg.lParam.0 & (1 << 30) == 0 {
                        command(hwnd, id);
                    }
                    continue;
                }
                if super::pin_toolbar::translate(&msg) {
                    continue;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            Ok(())
        }
    })();
    if STAGE.with(|s| s.borrow_mut().take()).is_some() {
        STAGING.store(0, Ordering::Release);
    }
    PICTURE.with(|p| {
        p.borrow_mut().take();
    });
    result
}

/// 单按 Ctrl（按下与松开之间没有其他按键或鼠标按键）时返回 true。
fn ctrl_tapped(msg: &MSG) -> bool {
    let control = msg.wParam.0 == usize::from(VK_CONTROL.0);
    match msg.message {
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            if !control {
                CTRL_TAP.set(false);
            } else if msg.lParam.0 & (1 << 30) == 0 {
                CTRL_TAP.set(true);
            }
            false
        }
        WM_KEYUP | WM_SYSKEYUP if control => CTRL_TAP.replace(false),
        WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_MBUTTONDOWN | WM_XBUTTONDOWN | WM_MOUSEWHEEL
        | WM_MOUSEHWHEEL | WM_NCLBUTTONDOWN | WM_NCRBUTTONDOWN => {
            CTRL_TAP.set(false);
            false
        }
        _ => false,
    }
}

/// 截图阶段 → 贴图：关闭暗色背景，换成贴图工具栏（默认收起），图片和标注保留。
fn to_pinned(hwnd: HWND) -> Result<()> {
    ink::drop_tool(hwnd)?;
    stop_drag(hwnd);
    if STAGE.with(|s| s.borrow_mut().take()).is_some() {
        STAGING.store(0, Ordering::Release);
    }
    with_picture(|p| -> Result<()> {
        p.handles = false;
        p.toolbar = false;
        p.surface = p.compose(p.view)?;
        Ok(())
    })?;
    render(hwnd, None)?;
    bar::destroy();
    bar::create(hwnd, with_picture(|p| p.alpha), false)?;
    ink::activate(hwnd);
    update_selection(hwnd)
}

/// 贴图阶段打开工具栏进入编辑。
fn open_toolbar(hwnd: HWND) -> Result<()> {
    if with_picture(|p| p.folded) {
        toggle_fold(hwnd)?;
    }
    with_picture(|p| p.toolbar = true);
    update_selection(hwnd)
}

/// 完成编辑：落定标注、取消工具并收起工具栏。
fn finish_editing(hwnd: HWND) -> Result<()> {
    ink::drop_tool(hwnd)?;
    with_picture(|p| p.toolbar = false);
    update_selection(hwnd)
}

/// 前台窗口属于本贴图（自身或其所属窗口）且未折叠时为选中，辉光变亮。
/// 截图阶段始终选中并显示工具栏；贴图阶段选中且按空格打开编辑时才显示工具栏。
fn update_selection(hwnd: HWND) -> Result<()> {
    let owned = unsafe {
        let foreground = GetForegroundWindow();
        !foreground.is_invalid()
            && (foreground == hwnd || GetAncestor(foreground, GA_ROOTOWNER) == hwnd)
    };
    let stage = staging();
    let (changed, show) = with_picture(|p| {
        let selected = stage || (owned && !p.folded);
        let show = selected && (stage || p.toolbar);
        if p.selected == selected {
            return (false, show);
        }
        p.selected = selected;
        paint_frame(&mut p.surface, selected);
        if p.handles {
            paint_handles(&mut p.surface, p.ink.scale);
        }
        if let Some(surface) = &mut p.zoom_surface {
            paint_frame(surface, selected);
        }
        (true, show)
    });
    bar::set_selected(show);
    if changed { render(hwnd, None) } else { Ok(()) }
}

fn point(lp: LPARAM) -> POINT {
    POINT {
        x: (lp.0 as u16) as i16 as i32,
        y: ((lp.0 >> 16) as u16) as i16 as i32,
    }
}
fn report_render_error(result: Result<()>) {
    if let Err(e) = result {
        log::error!("更新贴图失败: {e:#}");
    }
}

/// 默认文件名带本地时间，例如 NextGestures_2026-11-26_09-30-05.png。
fn default_file_name() -> String {
    let time = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    format!(
        "NextGestures_{:04}-{:02}-{:02}_{:02}-{:02}-{:02}.png",
        time.wYear, time.wMonth, time.wDay, time.wHour, time.wMinute, time.wSecond
    )
}

fn save_png(owner: HWND, bytes: &[u8]) -> Result<bool> {
    use windows::Win32::UI::Controls::Dialogs::*;
    use windows::core::{PCWSTR, PWSTR};
    let mut path = [0u16; 32768];
    let name: Vec<_> = default_file_name().encode_utf16().collect();
    path[..name.len()].copy_from_slice(&name);
    let filter: Vec<_> = "PNG 图片\0*.png\0\0".encode_utf16().collect();
    let mut dialog = OPENFILENAMEW {
        lStructSize: size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: owner,
        lpstrTitle: w!("保存贴图"),
        lpstrFile: PWSTR(path.as_mut_ptr()),
        nMaxFile: path.len() as u32,
        lpstrFilter: PCWSTR(filter.as_ptr()),
        lpstrDefExt: w!("png"),
        Flags: OFN_OVERWRITEPROMPT | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR,
        ..Default::default()
    };
    if !unsafe { GetSaveFileNameW(&mut dialog) }.as_bool() {
        let error = unsafe { CommDlgExtendedError() };
        ensure!(error.0 == 0, "保存对话框失败：{error:?}");
        return Ok(false);
    }
    let length = path.iter().position(|&c| c == 0).unwrap_or(path.len());
    std::fs::write(
        std::path::PathBuf::from(String::from_utf16_lossy(&path[..length])),
        bytes,
    )?;
    Ok(true)
}

/// 选择标注工具的命令（Ctrl+1~6 / E / 工具栏按钮）。
fn is_tool(id: u16) -> bool {
    matches!(
        id,
        bar::RECT_TOOL
            | bar::LINE_TOOL
            | bar::PEN
            | bar::MARKER
            | bar::MOSAIC
            | bar::TEXT
            | bar::ERASER
    )
}

fn command(hwnd: HWND, id: u16) {
    if PICTURE.with(|p| p.borrow().as_ref().is_none_or(|p| p.exporting)) {
        return;
    }
    let stage = staging();
    if stage {
        stop_drag(hwnd);
    }
    let result = (|| -> Result<()> {
        match id {
            bar::COPY => {
                ink::settle(hwnd)?;
                let (width, height, pixels) = PICTURE.with(|p| {
                    let p = p.borrow();
                    let p = p.as_ref().unwrap();
                    (p.width, p.height, p.pixels.clone())
                });
                super::clipboard::set_image_bgra(hwnd, width, height, &pixels)?;
                if stage {
                    // 截图阶段：复制后退出。
                    unsafe { DestroyWindow(hwnd)? };
                } else {
                    bar::feedback(bar::COPY);
                }
            }
            bar::SAVE => {
                ink::settle(hwnd)?;
                let bytes = PICTURE.with(|p| p.borrow().as_ref().unwrap().png())?;
                PICTURE.with(|p| p.borrow_mut().as_mut().unwrap().exporting = true);
                bar::busy(true);
                let saved = save_png(hwnd, &bytes);
                PICTURE.with(|p| p.borrow_mut().as_mut().unwrap().exporting = false);
                bar::busy(false);
                if saved? {
                    if stage {
                        unsafe { DestroyWindow(hwnd)? };
                    } else {
                        bar::feedback(bar::SAVE);
                    }
                }
            }
            bar::LESS | bar::MORE if !stage => {
                let alpha = PICTURE.with(|p| {
                    let mut p = p.borrow_mut();
                    let p = p.as_mut().unwrap();
                    p.alpha = (i32::from(p.alpha) + if id == bar::LESS { -13 } else { 13 })
                        .clamp(50, 255) as u8;
                    p.alpha
                });
                render(hwnd, None)?;
                bar::alpha(alpha);
            }
            bar::PIN if stage => to_pinned(hwnd)?,
            bar::CLOSE => unsafe {
                DestroyWindow(hwnd)?;
            },
            bar::DONE if !stage => finish_editing(hwnd)?,
            TOGGLE_TOOLBAR if !stage => {
                if with_picture(|p| p.toolbar) {
                    finish_editing(hwnd)?;
                } else {
                    open_toolbar(hwnd)?;
                }
            }
            TOGGLE_FOLD if !stage => {
                if with_picture(|p| p.toolbar) {
                    finish_editing(hwnd)?;
                } else {
                    ink::settle(hwnd)?;
                }
                toggle_fold(hwnd)?;
            }
            // Esc：先提交文字 / 取消草稿；截图阶段随后退出，贴图阶段编辑中为「完成」，否则关闭。
            bar::ESCAPE => {
                if ink::escape_edit(hwnd)? {
                    return Ok(());
                }
                if !stage && with_picture(|p| p.toolbar) {
                    finish_editing(hwnd)?;
                } else {
                    unsafe { DestroyWindow(hwnd)? };
                }
            }
            id if is_tool(id) && !stage && !with_picture(|p| p.toolbar) => {
                // 未打开工具栏时单键 E 不生效；Ctrl+数字直接进入编辑并选中工具。
                if id != bar::ERASER {
                    open_toolbar(hwnd)?;
                    ink::command(hwnd, id)?;
                }
            }
            bar::PIN | bar::DONE | bar::LESS | bar::MORE | TOGGLE_TOOLBAR | TOGGLE_FOLD => {}
            _ => {
                ink::command(hwnd, id)?;
            }
        }
        Ok(())
    })();
    if let Err(e) = result {
        log::error!("贴图操作失败: {e:#}");
        bar::error(&format!("{e:#}"));
    }
}

fn flash(hwnd: HWND, on: bool) {
    with_picture(|p| p.flash = on);
    unsafe {
        if on {
            SetTimer(Some(hwnd), FLASH_TIMER, FLASH_MS, None);
        } else {
            let _ = KillTimer(Some(hwnd), FLASH_TIMER);
        }
    }
    report_render_error(render(hwnd, None));
}

fn is_edge(code: usize) -> bool {
    [
        HTLEFT,
        HTRIGHT,
        HTTOP,
        HTTOPLEFT,
        HTTOPRIGHT,
        HTBOTTOM,
        HTBOTTOMLEFT,
        HTBOTTOMRIGHT,
    ]
    .contains(&(code as u32))
}

unsafe extern "system" fn procedure(hwnd: HWND, message: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match message {
            super::pin_toolbar::COMMAND => {
                command(hwnd, wp.0 as u16);
                LRESULT(0)
            }
            FLASH => {
                flash(hwnd, true);
                LRESULT(0)
            }
            WM_ACTIVATE => {
                // 激活切换完成后再判断前台窗口，工具栏 / 文字框 / 对话框都算选中。
                let _ = PostMessageW(Some(hwnd), bar::SELECTION, WPARAM(0), LPARAM(0));
                DefWindowProcW(hwnd, message, wp, lp)
            }
            bar::SELECTION => {
                report_render_error(update_selection(hwnd));
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                let result = if let Some(resize) = RESIZE.get() {
                    move_resize(hwnd, resize)
                } else if let Some(offset) = DRAG_OFFSET.get() {
                    move_drag(hwnd, offset)
                } else {
                    ink::pointer_move(hwnd, point(lp), wp.0 & 0x0004 != 0)
                };
                report_render_error(result);
                LRESULT(0)
            }
            WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
                if GetForegroundWindow() != hwnd {
                    ink::activate(hwnd);
                }
                report_render_error(ink::pointer_down(hwnd, point(lp)));
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                if staging() && (RESIZE.get().is_some() || DRAG_OFFSET.get().is_some()) {
                    let result = if let Some(resize) = RESIZE.get() {
                        move_resize(hwnd, resize)
                    } else if let Some(offset) = DRAG_OFFSET.get() {
                        move_drag(hwnd, offset)
                    } else {
                        Ok(())
                    };
                    stop_drag(hwnd);
                    report_render_error(result);
                } else if !stop_drag(hwnd) {
                    report_render_error(ink::pointer_up(hwnd));
                }
                LRESULT(0)
            }
            WM_CAPTURECHANGED => {
                DRAG_OFFSET.set(None);
                RESIZE.set(None);
                finish_crop();
                DefWindowProcW(hwnd, message, wp, lp)
            }
            WM_CANCELMODE => {
                stop_drag(hwnd);
                DefWindowProcW(hwnd, message, wp, lp)
            }
            WM_SETCURSOR
                if (lp.0 & 0xffff) as u32 == HTCLIENT && with_picture(|p| p.ink.tool.is_some()) =>
            {
                let mut cursor = POINT::default();
                if GetCursorPos(&mut cursor).is_ok() {
                    let _ = ScreenToClient(hwnd, &mut cursor);
                    if let Some(handle) = ink::cursor_for(hwnd, cursor) {
                        SetCursor(Some(handle));
                        return LRESULT(1);
                    }
                }
                DefWindowProcW(hwnd, message, wp, lp)
            }
            // 截图阶段可拖动选区时显示移动光标。
            WM_SETCURSOR if (lp.0 & 0xffff) as u32 == HTCAPTION && staging() => {
                SetCursor(LoadCursorW(None, IDC_SIZEALL).ok());
                LRESULT(1)
            }
            WM_CHAR if ink::editing() => {
                report_render_error(ink::char_input(hwnd, wp.0 as u16));
                LRESULT(0)
            }
            windows::Win32::UI::Controls::WM_MOUSELEAVE => {
                report_render_error(ink::mouse_leave(hwnd));
                LRESULT(0)
            }
            WM_TIMER if wp.0 == ink::CARET_TIMER => {
                report_render_error(ink::caret_tick(hwnd));
                LRESULT(0)
            }
            WM_TIMER if wp.0 == FLASH_TIMER => {
                flash(hwnd, false);
                LRESULT(0)
            }
            WM_TIMER if wp.0 == ZOOM_BADGE_TIMER => {
                if let Some((_, origin)) = ZOOM_BADGE.get() {
                    let mut cursor = POINT::default();
                    if GetCursorPos(&mut cursor).is_ok() && cursor != origin {
                        hide_zoom_badge(hwnd);
                    }
                }
                LRESULT(0)
            }
            WM_WINDOWPOSCHANGED => {
                if let Err(e) = super::pin_toolbar::position() {
                    log::warn!("贴图工具栏定位失败: {e}");
                }
                DefWindowProcW(hwnd, message, wp, lp)
            }
            WM_NCHITTEST => hit_test(hwnd, point(lp)),
            WM_NCLBUTTONDOWN if wp.0 == HTCAPTION as usize => {
                if GetForegroundWindow() != hwnd {
                    ink::activate(hwnd);
                }
                report_render_error(start_drag(hwnd, point(lp)));
                LRESULT(0)
            }
            WM_NCLBUTTONDOWN if is_edge(wp.0) && staging() => {
                report_render_error(start_resize(hwnd, wp.0 as u32, point(lp)));
                LRESULT(0)
            }
            // 双击：截图阶段复制并退出（类 Snipaste），贴图阶段关闭。
            WM_NCLBUTTONDBLCLK if wp.0 == HTCAPTION as usize => {
                command(hwnd, if staging() { bar::COPY } else { bar::CLOSE });
                LRESULT(0)
            }
            WM_NCLBUTTONDBLCLK => LRESULT(0),
            WM_NCRBUTTONDOWN | WM_RBUTTONDOWN => LRESULT(0),
            // 涂鸦模式右键：结束折线 / 提交文字 / 退出涂鸦，不关闭贴图。
            WM_RBUTTONUP if with_picture(|p| p.ink.tool.is_some()) => {
                let result = ink::right_click(hwnd);
                report_render_error(result);
                LRESULT(0)
            }
            WM_NCRBUTTONUP | WM_RBUTTONUP | WM_CLOSE => {
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_KEYDOWN if wp.0 == 27 => {
                command(hwnd, bar::ESCAPE);
                LRESULT(0)
            }

            WM_MOUSEWHEEL => {
                let delta = ((wp.0 >> 16) as i16) as i32;
                report_render_error(ink::wheel(hwnd, delta).and_then(|handled| {
                    if handled {
                        Ok(())
                    } else {
                        zoom_picture(hwnd, delta, point(lp))
                    }
                }));
                LRESULT(0)
            }
            WM_DESTROY => {
                stop_drag(hwnd);
                hide_zoom_badge(hwnd);
                let _ = KillTimer(Some(hwnd), FLASH_TIMER);
                // 截图阶段退出时关闭暗色背景；贴图关闭后压入历史栈供贴图键恢复。
                if STAGE.with(|s| s.borrow_mut().take()).is_some() {
                    STAGING.store(0, Ordering::Release);
                } else {
                    push_closed(hwnd);
                }
                let _ = RemovePropW(hwnd, w!("NextGesturesQuickPin"));
                PostQuitMessage(0);
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
    fn raw_gesture_bounds_preserve_negative_monitor_coordinates() {
        assert_eq!(
            Bounds::from_points(&[
                (-900, 100),
                (-900, 400),
                (-500, 400),
                (-500, 100),
                (-900, 100)
            ])
            .unwrap(),
            Bounds {
                x: -900,
                y: 100,
                width: 400,
                height: 300
            }
        );
        assert!(Bounds::from_points(&[]).is_err());
        assert!(Bounds::from_points(&[(0, 0), (10, 200)]).is_err());
        assert!(Bounds::from_points(&[(i32::MIN, 0), (i32::MAX, 100)]).is_err());
    }
    #[test]
    fn composed_shadow_is_transparent_at_outer_corners_and_soft_at_edges() {
        assert_eq!(shadow_alpha(0, 0, 200, 100), 0);
        assert_eq!(shadow_alpha(239, 139, 200, 100), 0);
        assert!(shadow_alpha(19, 70, 200, 100) > shadow_alpha(10, 70, 200, 100));
        assert!(shadow_alpha(19, 70, 200, 100) < 255);
        assert_eq!(edge_pixel(0, 0, 200, 100, true), [0; 4]);
        let edge = edge_pixel(19, 70, 200, 100, true);
        assert!(edge[0] > edge[1] && edge[1] > edge[2]);
        assert!(edge[..3].iter().all(|&channel| channel <= edge[3]));
        assert!(edge[0] > edge_pixel(12, 70, 200, 100, true)[0]);
        let [blue, green, red, alpha] = edge_pixel(19, 70, 200, 100, false);
        assert!(blue == green && green == red && red > 0 && alpha >= red);
    }

    #[test]
    fn zoomed_surface_keeps_content_opaque_and_shadow_unscaled() {
        let mut source = Surface::new(16 + 2 * SHADOW, 12 + 2 * SHADOW).unwrap();
        let stride = source.width as usize * 4;
        for row in source
            .bytes_mut()
            .chunks_exact_mut(stride)
            .skip(SHADOW as usize)
            .take(12)
        {
            for pixel in row[SHADOW as usize * 4..][..16 * 4].chunks_exact_mut(4) {
                pixel.copy_from_slice(&[40, 80, 120, 255]);
            }
        }
        let size = zoom_size(16, 12, 2.0);
        let mut target = scaled_surface(&source, size, false).unwrap().unwrap();
        assert_eq!(
            (target.width, target.height),
            (32 + 2 * SHADOW, 24 + 2 * SHADOW)
        );
        let first = (target.width + 1) as usize * SHADOW as usize * 4;
        assert_eq!(target.bytes_mut()[first..first + 4], [40, 80, 120, 255]);
        assert_eq!(target.bytes_mut()[..4], [0; 4]);
        assert_eq!(
            source.bytes_mut()[(stride + 4) * SHADOW as usize..][..4],
            [40, 80, 120, 255]
        );
    }

    #[test]
    fn png_export_keeps_full_original_pixels_when_folded_and_translucent() {
        let picture = Picture {
            pixels: vec![0, 0, 255, 255, 255, 0, 0, 255],
            width: 1,
            height: 2,
            view: View {
                x: 0,
                y: 1,
                width: 1,
                height: 1,
            },
            folded: true,
            surface: Surface::new(1, 1).unwrap(),
            zoom: 1.0,
            zoom_surface: None,
            zoom_wheel_remainder: 0,
            alpha: 50,
            wheel_remainder: 0,
            exporting: false,
            selected: false,
            ink: ink::Annotate::default(),
            toolbar: false,
            flash: false,
            handles: false,
        };
        let bytes = picture.png().unwrap();
        let mut reader = png::Decoder::new(std::io::Cursor::new(bytes))
            .read_info()
            .unwrap();
        let mut pixels = vec![0; reader.output_buffer_size().unwrap()];
        let info = reader.next_frame(&mut pixels).unwrap();
        assert_eq!((info.width, info.height), (1, 2));
        assert_eq!(pixels, vec![255, 0, 0, 255, 0, 0, 255, 255]);
    }
    #[test]
    #[ignore = "opens the native screenshot selector and moves the desktop cursor; run alone"]
    fn desktop_screenshot_toolbar_tracks_crop() {
        use std::time::{Duration, Instant};
        use windows::core::PCWSTR;

        fn find(class: PCWSTR) -> HWND {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if let Ok(hwnd) = unsafe { FindWindowW(class, None) }
                    && unsafe { IsWindowVisible(hwnd).as_bool() }
                {
                    return hwnd;
                }
                assert!(Instant::now() < deadline, "expected window did not appear");
                std::thread::sleep(Duration::from_millis(10));
            }
        }

        fn pack(x: i32, y: i32) -> LPARAM {
            LPARAM((u32::from(x as u16) | (u32::from(y as u16) << 16)) as isize)
        }

        fn bounds(hwnd: HWND) -> RECT {
            let mut rect = RECT::default();
            unsafe { GetWindowRect(hwnd, &mut rect) }.unwrap();
            rect
        }

        fn desktop_pixel(x: i32, y: i32) -> COLORREF {
            unsafe {
                windows::Win32::Graphics::Dwm::DwmFlush().unwrap();
                let dc = GetDC(None);
                assert!(!dc.is_invalid());
                let pixel = GetPixel(dc, x, y);
                ReleaseDC(None, dc);
                assert_ne!(pixel.0, u32::MAX);
                pixel
            }
        }

        let worker = std::thread::spawn(|| screenshot(|| {}));
        unsafe {
            SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            let mut original_cursor = POINT::default();
            GetCursorPos(&mut original_cursor).unwrap();
            let selector = find(w!("NextGesturesRegionSelector"));
            let desktop = bounds(selector);
            let mut info = MONITORINFO {
                cbSize: size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            let monitor = MonitorFromPoint(POINT::default(), MONITOR_DEFAULTTOPRIMARY);
            assert!(GetMonitorInfoW(monitor, &mut info).as_bool());
            let x = (info.rcWork.left + info.rcWork.right) / 2 - 150;
            let y = (info.rcWork.top + info.rcWork.bottom) / 2 - 150;
            let a = pack(x - desktop.left, y - desktop.top);
            for (msg, wp, lp) in [
                (WM_LBUTTONDOWN, 1, a),
                (WM_MOUSEMOVE, 1, pack(x + 270 - desktop.left, y + 190 - desktop.top)),
                (WM_LBUTTONUP, 0, pack(x + 300 - desktop.left, y + 200 - desktop.top)),
            ] {
                PostMessageW(Some(selector), msg, WPARAM(wp), lp).unwrap();
            }

            let hwnd = find(w!("NextGesturesQuickPin"));
            let toolbar = find(w!("NextGesturesPinToolbar"));
            let thread = GetWindowThreadProcessId(hwnd, None);
            let frame = HWND(GetPropW(toolbar, w!("NextGesturesToolbarFrame")).0);
            assert!(!frame.is_invalid());
            let initial_image = image_rect(hwnd).unwrap();
            let initial_toolbar = bounds(toolbar);
            assert_eq!((initial_image.left, initial_image.top), (x, y));
            let sample = (x + 180, y + 120);
            let initial_pixel = desktop_pixel(sample.0, sample.1);

            let drag = |hit: u32, from: POINT, to: POINT| {
                SetCursorPos(from.x, from.y).unwrap();
                SendMessageW(hwnd, WM_NCLBUTTONDOWN, Some(WPARAM(hit as usize)), Some(pack(from.x, from.y)));
                let mut info = GUITHREADINFO {
                    cbSize: size_of::<GUITHREADINFO>() as u32,
                    ..Default::default()
                };
                GetGUIThreadInfo(thread, &mut info).unwrap();
                assert_eq!(info.hwndCapture, hwnd, "drag must capture the cursor");
                for step in 1..=8 {
                    SetCursorPos(
                        from.x + (to.x - from.x) * step / 8,
                        from.y + (to.y - from.y) * step / 8,
                    )
                    .unwrap();
                    SendMessageW(hwnd, WM_MOUSEMOVE, Some(WPARAM(1)), Some(LPARAM(0)));
                    assert_eq!(
                        desktop_pixel(sample.0, sample.1),
                        initial_pixel,
                        "the same desktop pixel stays in place through the drag"
                    );
                }
                let image = image_rect(hwnd).unwrap();
                let bar = bounds(toolbar);
                assert_eq!(bounds(frame), bar);
                SendMessageW(hwnd, WM_LBUTTONUP, None, None);
                assert_eq!(image_rect(hwnd).unwrap(), image);
                assert_eq!(bounds(toolbar), bar);
                (image, bar)
            };
            let (moved, moved_bar) = drag(
                HTCAPTION,
                POINT { x: x + 100, y: y + 100 },
                POINT { x: x + 140, y: y + 125 },
            );
            assert_eq!((moved.left, moved.top), (x + 40, y + 25));
            assert_eq!(moved_bar.left - initial_toolbar.left, 40);
            assert_eq!(moved_bar.top - initial_toolbar.top, 25);
            let (resized, resized_bar) = drag(
                HTBOTTOMRIGHT,
                POINT { x: moved.right - 1, y: moved.bottom - 1 },
                POINT { x: moved.right + 59, y: moved.bottom + 29 },
            );
            assert_eq!((resized.right - moved.right, resized.bottom - moved.bottom), (60, 30));
            assert_eq!(resized_bar.left - moved_bar.left, 30);
            assert_eq!(resized_bar.top - moved_bar.top, 30);
            PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0)).unwrap();
            SetCursorPos(original_cursor.x, original_cursor.y).unwrap();
        }
        worker.join().unwrap().unwrap();
    }

    #[test]
    #[ignore = "opens a real desktop pin window; run alone"]
    fn desktop_pin_toolbar_space_fold_opacity_save_cancel_and_double_click_close() {
        use std::time::{Duration, Instant};
        use windows::Win32::UI::Input::KeyboardAndMouse::{
            INPUT, INPUT_0, INPUT_MOUSE, MOUSE_EVENT_FLAGS, MOUSEEVENTF_LEFTDOWN,
            MOUSEEVENTF_LEFTUP, MOUSEINPUT, SendInput,
        };

        fn left_button(flags: MOUSE_EVENT_FLAGS) {
            let input = INPUT {
                r#type: INPUT_MOUSE,
                Anonymous: INPUT_0 {
                    mi: MOUSEINPUT {
                        dwFlags: flags,
                        ..Default::default()
                    },
                },
            };
            assert_eq!(unsafe { SendInput(&[input], size_of::<INPUT>() as i32) }, 1);
        }
        unsafe extern "system" fn backdrop_proc(
            hwnd: HWND,
            msg: u32,
            wp: WPARAM,
            lp: LPARAM,
        ) -> LRESULT {
            unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
        }
        unsafe {
            SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        }
        // 已知黑色源图和白色背景，用桌面合成后的真实像素验证透明度及阴影。
        let backdrop = unsafe {
            for (name, brush) in [
                (w!("PinTestWhite"), WHITE_BRUSH),
                (w!("PinTestBlack"), BLACK_BRUSH),
            ] {
                assert_ne!(
                    RegisterClassW(&WNDCLASSW {
                        lpfnWndProc: Some(backdrop_proc),
                        lpszClassName: name,
                        hbrBackground: HBRUSH(GetStockObject(brush).0),
                        ..Default::default()
                    }),
                    0
                );
            }
            let background = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
                w!("PinTestWhite"),
                w!("Pin test backdrop"),
                WS_POPUP | WS_VISIBLE,
                70,
                70,
                500,
                400,
                None,
                None,
                None,
                None,
            )
            .unwrap();
            let source = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                w!("PinTestBlack"),
                w!(""),
                WS_CHILD | WS_VISIBLE,
                30,
                30,
                240,
                160,
                Some(background),
                None,
                None,
                None,
            )
            .unwrap();
            let _ = UpdateWindow(background);
            let _ = UpdateWindow(source);
            (background, source)
        };
        fn gray(x: i32, y: i32) -> u32 {
            unsafe {
                let dc = GetDC(None);
                let pixel = GetPixel(dc, x, y);
                ReleaseDC(None, dc);
                pixel.0
            }
        }
        fn await_gray(x: i32, y: i32, expected: i32) -> u32 {
            let deadline = Instant::now() + Duration::from_secs(3);
            loop {
                let rgb = gray(x, y);
                if [rgb & 255, (rgb >> 8) & 255, (rgb >> 16) & 255]
                    .iter()
                    .all(|v| (*v as i32 - expected).abs() <= 2)
                    || Instant::now() >= deadline
                {
                    return rgb;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        assert_eq!(await_gray(150, 150, 0), 0);
        let worker = std::thread::spawn(|| {
            open(Bounds {
                x: 100,
                y: 100,
                width: 240,
                height: 160,
            })
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        let hwnd = loop {
            if let Ok(hwnd) = unsafe { FindWindowW(w!("NextGesturesQuickPin"), None) } {
                if unsafe { IsWindowVisible(hwnd) }.as_bool() {
                    break hwnd;
                }
            }
            assert!(Instant::now() < deadline, "pin did not open");
            std::thread::sleep(Duration::from_millis(25));
        };
        fn packed(x: i32, y: i32) -> LPARAM {
            LPARAM(((y as u16 as u32) << 16 | x as u16 as u32) as isize)
        }
        let evidence = unsafe {
            DestroyWindow(backdrop.1).unwrap();
            let initial_pixel = await_gray(150, 150, 0);
            // 贴图立即激活；工具栏默认收起，按空格打开。
            let toolbar = FindWindowW(w!("NextGesturesPinToolbar"), None).unwrap();
            ink::activate(hwnd);
            assert_eq!(GetForegroundWindow(), hwnd);
            SendMessageW(hwnd, bar::SELECTION, None, None);
            std::thread::sleep(Duration::from_millis(100));
            assert!(!IsWindowVisible(toolbar).as_bool(), "toolbar should wait for Space");
            PostMessageW(Some(hwnd), WM_KEYDOWN, WPARAM(usize::from(VK_SPACE.0)), LPARAM(1))
                .unwrap();
            windows::Win32::Graphics::Dwm::DwmFlush().unwrap();
            let shadow_pixel = gray(99, 180);
            let deadline = Instant::now() + Duration::from_secs(2);
            while !IsWindowVisible(toolbar).as_bool() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            let toolbar_visible = IsWindowVisible(toolbar).as_bool();
            let mut toolbar_rect = RECT::default();
            GetWindowRect(toolbar, &mut toolbar_rect).unwrap();
            let toolbar_native = is_at((toolbar_rect.left + 20, toolbar_rect.top + 15));
            SendMessageW(
                GetDlgItem(Some(toolbar), i32::from(super::super::pin_toolbar::LESS)).unwrap(),
                BM_CLICK,
                None,
                None,
            );
            let lower_opacity_pixel = await_gray(150, 150, 13);
            SendMessageW(
                GetDlgItem(Some(toolbar), i32::from(super::super::pin_toolbar::MORE)).unwrap(),
                BM_CLICK,
                None,
                None,
            );
            let opaque_again = await_gray(150, 150, 0);
            // Keep the fixture beneath the topmost pin when it takes focus.
            SetWindowPos(
                backdrop.0,
                Some(HWND_NOTOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            )
            .unwrap();
            ink::activate(backdrop.0);
            assert_eq!(GetForegroundWindow(), backdrop.0);
            let deadline = Instant::now() + Duration::from_secs(2);
            while IsWindowVisible(toolbar).as_bool() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(!IsWindowVisible(toolbar).as_bool());
            windows::Win32::Graphics::Dwm::DwmFlush().unwrap();
            let idle_edge = gray(99, 180);
            assert_eq!(idle_edge & 255, (idle_edge >> 8) & 255);
            assert_eq!(idle_edge & 255, (idle_edge >> 16) & 255);
            assert!((100..250).contains(&(idle_edge & 255)));
            assert_eq!(WindowFromPoint(POINT { x: 150, y: 150 }), hwnd);
            PostMessageW(
                Some(hwnd),
                WM_NCLBUTTONDOWN,
                WPARAM(HTCAPTION as usize),
                packed(150, 150),
            )
            .unwrap();
            PostMessageW(Some(hwnd), WM_LBUTTONUP, WPARAM(0), packed(150, 150)).unwrap();
            let deadline = Instant::now() + Duration::from_secs(2);
            while !IsWindowVisible(toolbar).as_bool() && Instant::now() < deadline {
                // The backdrop belongs to this thread and must process its deactivation.
                let mut message = MSG::default();
                while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                    DispatchMessageW(&message);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            assert_eq!(GetForegroundWindow(), hwnd);
            assert!(IsWindowVisible(toolbar).as_bool());
            windows::Win32::Graphics::Dwm::DwmFlush().unwrap();
            let active_edge = gray(99, 180);
            assert!(((active_edge >> 16) & 255) > (active_edge & 255) + 30);
            SendMessageW(
                GetDlgItem(Some(toolbar), i32::from(bar::LESS)).unwrap(),
                BM_CLICK,
                None,
                None,
            );
            assert!((await_gray(150, 150, 13) & 255).abs_diff(13) <= 2);
            SendMessageW(
                GetDlgItem(Some(toolbar), i32::from(bar::MORE)).unwrap(),
                BM_CLICK,
                None,
                None,
            );
            assert!(await_gray(150, 150, 0) & 255 <= 2);
            if let Ok(directory) = std::env::var("NEXTGESTURES_PIN_EVIDENCE") {
                let mut snapshot = capture(Bounds {
                    x: 70,
                    y: 70,
                    width: 350,
                    height: 260,
                })
                .unwrap();
                for pixel in snapshot.pixels.chunks_exact_mut(4) {
                    pixel.swap(0, 2);
                }
                std::fs::create_dir_all(&directory).unwrap();
                let file =
                    std::fs::File::create(std::path::Path::new(&directory).join("pin-shadow.png"))
                        .unwrap();
                let mut encoder = png::Encoder::new(file, 350, 260);
                encoder.set_color(png::ColorType::Rgba);
                encoder.set_depth(png::BitDepth::Eight);
                encoder
                    .write_header()
                    .unwrap()
                    .write_image_data(&snapshot.pixels)
                    .unwrap();
            }
            let topmost = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) & WS_EX_TOPMOST.0 as isize != 0;
            let hit = SendMessageW(hwnd, WM_NCHITTEST, None, Some(packed(150, 150))).0;
            let shadow_hit = SendMessageW(hwnd, WM_NCHITTEST, None, Some(packed(81, 150))).0;
            let own = is_at((150, 150));
            let mut original = RECT::default();
            GetWindowRect(hwnd, &mut original).unwrap();
            // 单按 Ctrl（含长按产生的重复按下）切换折叠一次。
            let tap_ctrl = || {
                let ctrl = WPARAM(usize::from(VK_CONTROL.0));
                PostMessageW(Some(hwnd), WM_KEYDOWN, ctrl, LPARAM(1)).unwrap();
                PostMessageW(Some(hwnd), WM_KEYDOWN, ctrl, LPARAM(1 | 1 << 30)).unwrap();
                PostMessageW(Some(hwnd), WM_KEYUP, ctrl, LPARAM(1 | 3 << 30)).unwrap();
            };
            let await_width = |width| {
                let deadline = Instant::now() + Duration::from_secs(2);
                loop {
                    let mut rect = RECT::default();
                    GetWindowRect(hwnd, &mut rect).unwrap();
                    if rect.right - rect.left == width || Instant::now() >= deadline {
                        return rect;
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
            };
            tap_ctrl();
            let folded = await_width(104);
            SetWindowPos(
                hwnd,
                None,
                folded.left + 30,
                folded.top + 40,
                0,
                0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            )
            .unwrap();
            tap_ctrl();
            let restored = await_width(280);
            let cursor = packed(170, 180);
            SendMessageW(
                hwnd,
                WM_MOUSEWHEEL,
                Some(WPARAM((-120i16 as u16 as usize) << 16)),
                Some(cursor),
            );
            let zoomed = await_width(258);
            assert_eq!(
                (zoomed.left, zoomed.top, zoomed.bottom - zoomed.top),
                (114, 124, 185)
            );
            let pixel = await_gray(170, 180, 0);
            SendMessageW(hwnd, WM_MOUSEWHEEL, Some(WPARAM(120 << 16)), Some(cursor));
            assert_eq!(await_width(280), restored);
            let badge = FindWindowW(w!("NextGesturesPinZoom"), w!("100%")).unwrap();
            let mut badge_rect = RECT::default();
            GetWindowRect(badge, &mut badge_rect).unwrap();
            assert!(badge_rect.left >= restored.left + SHADOW);
            assert!(badge_rect.top >= restored.top + SHADOW);
            windows::Win32::Graphics::Dwm::DwmFlush().unwrap();
            let badge_background = gray(badge_rect.left + 2, badge_rect.top + 2);
            assert!((20..110).contains(&(badge_background & 255)));
            let mut cursor = POINT::default();
            GetCursorPos(&mut cursor).unwrap();
            let test_x = if cursor.x == 150 { 151 } else { 150 };
            SetCursorPos(test_x, 150).unwrap();
            let deadline = Instant::now() + Duration::from_secs(1);
            while IsWindow(Some(badge)).as_bool() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            SetCursorPos(cursor.x, cursor.y).unwrap();
            assert!(!IsWindow(Some(badge)).as_bool());
            SendMessageW(
                GetDlgItem(Some(toolbar), i32::from(super::super::pin_toolbar::SAVE)).unwrap(),
                BM_CLICK,
                None,
                None,
            );
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut cancelled = false;
            loop {
                if let Ok(dialog) = FindWindowW(w!("#32770"), w!("保存贴图")) {
                    if GetWindow(dialog, GW_OWNER).ok() == Some(hwnd) {
                        SendMessageW(dialog, WM_CLOSE, None, None);
                        cancelled = true;
                        break;
                    }
                }
                if Instant::now() >= deadline {
                    break;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            let mut drag_positions = None;
            if cancelled {
                let deadline = Instant::now() + Duration::from_secs(3);
                while !IsWindowVisible(toolbar).as_bool() && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(20));
                }
                let mut monitor = MONITORINFO {
                    cbSize: size_of::<MONITORINFO>() as u32,
                    ..Default::default()
                };
                assert!(
                    GetMonitorInfoW(
                        MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST),
                        &mut monitor
                    )
                    .as_bool()
                );
                let mut previous_cursor = POINT::default();
                GetCursorPos(&mut previous_cursor).unwrap();
                let x = restored.left + 90;
                let y = monitor.rcMonitor.top + 3;
                SetCursorPos(x, restored.top + 100).unwrap();
                left_button(MOUSEEVENTF_LEFTDOWN);
                let thread = GetWindowThreadProcessId(hwnd, None);
                let deadline = Instant::now() + Duration::from_secs(2);
                loop {
                    let mut info = GUITHREADINFO {
                        cbSize: size_of::<GUITHREADINFO>() as u32,
                        ..Default::default()
                    };
                    if GetGUIThreadInfo(thread, &mut info).is_ok() && info.hwndCapture == hwnd
                        || Instant::now() >= deadline
                    {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                SetCursorPos(x, y).unwrap();
                let expected_top = y - 100;
                let mut during_drag = RECT::default();
                let deadline = Instant::now() + Duration::from_secs(2);
                loop {
                    GetWindowRect(hwnd, &mut during_drag).unwrap();
                    if during_drag.top == expected_top || Instant::now() >= deadline {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                left_button(MOUSEEVENTF_LEFTUP);
                std::thread::sleep(Duration::from_millis(150));
                let mut after_release = RECT::default();
                GetWindowRect(hwnd, &mut after_release).unwrap();
                drag_positions = Some((during_drag.top, after_release.top, expected_top));
                SetCursorPos(x, y + 30).unwrap();
                for _ in 0..2 {
                    left_button(MOUSEEVENTF_LEFTDOWN);
                    left_button(MOUSEEVENTF_LEFTUP);
                    std::thread::sleep(Duration::from_millis(50));
                }
                SetCursorPos(previous_cursor.x, previous_cursor.y).unwrap();
            } else {
                SendMessageW(hwnd, WM_CLOSE, None, None);
            }
            let deadline = Instant::now() + Duration::from_secs(2);
            while IsWindow(Some(hwnd)).as_bool() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            let double_click_closed = !IsWindow(Some(hwnd)).as_bool();
            if !double_click_closed {
                SendMessageW(hwnd, WM_CLOSE, None, None);
            }
            (
                topmost,
                hit,
                shadow_hit,
                own,
                original,
                folded,
                restored,
                pixel,
                initial_pixel,
                shadow_pixel,
                toolbar_visible,
                toolbar_rect,
                lower_opacity_pixel,
                opaque_again,
                cancelled,
                toolbar,
                double_click_closed,
                toolbar_native,
                drag_positions,
            )
        };
        worker.join().unwrap().unwrap();
        unsafe {
            DestroyWindow(backdrop.0).unwrap();
        }
        assert!(evidence.0);
        assert_eq!(evidence.1, HTCAPTION as isize);
        assert_eq!(evidence.2, HTTRANSPARENT as isize);
        assert!(evidence.3);
        assert_eq!(
            (
                evidence.4.left,
                evidence.4.top,
                evidence.4.right - evidence.4.left,
                evidence.4.bottom - evidence.4.top
            ),
            (80, 80, 280, 200)
        );
        assert_eq!(
            (
                evidence.5.right - evidence.5.left,
                evidence.5.bottom - evidence.5.top
            ),
            (104, 104)
        );
        assert_eq!(
            (
                evidence.6.left,
                evidence.6.top,
                evidence.6.right - evidence.6.left,
                evidence.6.bottom - evidence.6.top
            ),
            (110, 120, 280, 200)
        );
        assert!(
            (evidence.7 & 255) <= 2,
            "zoomed opacity pixel: {:#x}",
            evidence.7
        );
        assert!(
            (evidence.8 & 255) <= 2,
            "initial opacity pixel: {:#x}",
            evidence.8
        );
        assert!(
            (1..250).contains(&(evidence.9 & 255)),
            "shadow pixel: {:#x}",
            evidence.9
        );
        assert!(
            ((evidence.9 >> 16) & 255) > (evidence.9 & 255) + 30,
            "blue edge glow pixel: {:#x}",
            evidence.9
        );
        assert_eq!(COUNT.load(Ordering::Relaxed), 0);
        assert!(evidence.10);
        assert!(evidence.11.top >= 260);
        assert!((evidence.12 & 255).abs_diff(13) <= 2);
        assert!((evidence.13 & 255) <= 2);
        assert!(evidence.14, "save dialog did not cancel cleanly");
        assert!(!unsafe { IsWindow(Some(evidence.15)) }.as_bool());
        assert!(evidence.16, "double-click did not close the pin");
        assert!(
            evidence.17,
            "toolbar controls did not bypass the gesture hook"
        );
        let (during_drag, after_release, expected_top) = evidence.18.unwrap();
        assert_eq!(
            during_drag, expected_top,
            "pin did not follow the cursor above the monitor"
        );
        assert_eq!(after_release, expected_top, "pin jumped down on release");
        println!(
            "Desktop pin: wheel zoom, toolbar opacity, fold, save cancel, drag, and double-click verified"
        );
    }
}
