//! 物理像素截图与逐像素阴影；每张贴图独立拥有图片及消息循环。
use anyhow::{Result, ensure};
use std::cell::{Cell, RefCell};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};
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

#[path = "pin_ink.rs"]
mod ink;
use ink::with_picture;

pub(super) const SHADOW: i32 = 20;
const FOLD_SIZE: i32 = 64;
const TOGGLE_FOLD: u16 = 151;
const ZOOM_STEP: f64 = 1.1;
const MAX_IMAGE_SIDE: i32 = 16384;
const MAX_IMAGE_PIXELS: i32 = 32 * 1024 * 1024;
const ZOOM_BADGE_TIMER: usize = 8;

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
}

impl Picture {
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
        let stride = surface.width as usize * 4;
        let bytes = surface.bytes_mut();
        for y in 0..view.height {
            let src = ((view.y + y) as usize * self.width as usize + view.x as usize) * 4;
            let dst = (y + SHADOW) as usize * stride + SHADOW as usize * 4;
            bytes[dst..dst + view.width as usize * 4]
                .copy_from_slice(&self.pixels[src..src + view.width as usize * 4]);
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

fn paint_frame(surface: &mut Surface, selected: bool) {
    let (width, height) = (surface.width - 2 * SHADOW, surface.height - 2 * SHADOW);
    let stride = surface.width as usize * 4;
    let bytes = surface.bytes_mut();
    for y in 0..height + 2 * SHADOW {
        let columns = if (SHADOW..SHADOW + height).contains(&y) {
            [0..SHADOW, width + SHADOW..width + 2 * SHADOW]
        } else {
            [0..width + 2 * SHADOW, 0..0]
        };
        for x in columns.into_iter().flatten() {
            let i = y as usize * stride + x as usize * 4;
            bytes[i..i + 4].copy_from_slice(&edge_pixel(x, y, width, height, selected));
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

thread_local! {
    static PICTURE: RefCell<Option<Picture>> = const { RefCell::new(None) };
    static DRAG_OFFSET: Cell<Option<POINT>> = const { Cell::new(None) };
    static ZOOM_BADGE: Cell<Option<(HWND, POINT)>> = const { Cell::new(None) };
}
static COUNT: AtomicUsize = AtomicUsize::new(0);
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
    let view = View {
        x: 0,
        y: 0,
        width: b.width,
        height: b.height,
    };
    let mut picture = Picture {
        pixels,
        width: b.width,
        height: b.height,
        view,
        folded: false,
        surface: raw,
        zoom: 1.0,
        zoom_surface: None,
        zoom_wheel_remainder: 0,
        alpha: 255,
        wheel_remainder: 0,
        exporting: false,
        selected: false,
        ink: ink::Annotate::default(),
    };
    picture.ink = ink::Annotate::new(picture.pixels.clone(), bar::SWATCHES[0]);
    picture.surface = picture.compose(view)?;
    Ok(picture)
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
            p.alpha,
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
    if DRAG_OFFSET.get().is_some() {
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

fn hit_test(hwnd: HWND, point: POINT) -> LRESULT {
    let mut rect = RECT::default();
    if unsafe { GetWindowRect(hwnd, &mut rect) }.is_err() {
        return LRESULT(HTTRANSPARENT as isize);
    }
    let (width, height, drawing) = PICTURE.with(|p| {
        let p = p.borrow();
        let p = p.as_ref().unwrap();
        let (width, height) = p.display_size();
        (width, height, p.ink.tool.is_some() && !p.folded)
    });
    if (rect.left + SHADOW..rect.left + SHADOW + width).contains(&point.x)
        && (rect.top + SHADOW..rect.top + SHADOW + height).contains(&point.y)
    {
        // 涂鸦模式走客户区；普通模式用标题区消息区分拖动和双击。
        LRESULT(if drawing { HTCLIENT } else { HTCAPTION } as isize)
    } else {
        LRESULT(HTTRANSPARENT as isize)
    }
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
    let dragging = DRAG_OFFSET.take().is_some();
    unsafe {
        if dragging && GetCapture() == hwnd {
            let _ = ReleaseCapture();
        }
    }
    dragging
}

pub fn open(bounds: Bounds) -> Result<()> {
    open_after_capture(bounds, || {})
}

/// 框选后复用贴图窗口及标注工具栏。应在独立工作线程调用。
pub fn select_and_open() -> Result<()> {
    select_and_open_after_capture(|| {})
}

pub fn select_and_open_after_capture(captured: impl FnOnce()) -> Result<()> {
    let region = super::region::select_region().map_err(anyhow::Error::msg)?;
    if let Some((x, y, width, height)) = region {
        let bounds = Bounds::from_points(&[(x, y), (x + width, y + height)])?;
        open_after_capture(bounds, captured)
    } else {
        Ok(())
    }
}

pub(super) fn activate_native_window(hwnd: HWND) {
    ink::activate(hwnd);
}

/// 截取静态图片后立即通知调用方恢复轨迹层，不等贴图窗口关闭。
pub fn open_after_capture(bounds: Bounds, captured: impl FnOnce()) -> Result<()> {
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
    let picture = capture(bounds);
    captured();
    let picture = picture?;
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
    PICTURE.with(|p| *p.borrow_mut() = Some(picture));
    let result = (|| -> Result<()> {
        unsafe {
            let hwnd = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED,
                w!("NextGesturesQuickPin"),
                w!("贴图 · 拖动移动 / 滚轮缩放 / 空格折叠或恢复 / 双击关闭"),
                WS_POPUP,
                bounds.x - SHADOW,
                bounds.y - SHADOW,
                bounds.width + SHADOW * 2,
                bounds.height + SHADOW * 2,
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
                render(
                    hwnd,
                    Some(POINT {
                        x: bounds.x - SHADOW,
                        y: bounds.y - SHADOW,
                    }),
                )?;
                super::pin_toolbar::create(hwnd, 255)?;
                let scale = GetDpiForWindow(hwnd).max(96) as f32 / 96.0;
                with_picture(|p| p.ink.scale = scale);
                Ok(())
            })() {
                let _ = DestroyWindow(hwnd);
                return Err(e);
            }
            let _ = ShowWindow(hwnd, SW_SHOW);
            // 截图完成即选中：辉光变亮、工具栏出现，可直接涂鸦或 Ctrl+C。
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
                // 贴图或其工具栏获得焦点时的快捷键（类似 Snipaste）。
                if let Some(id) = ink::shortcut(&msg) {
                    // WM_KEYDOWN 的第 30 位表示按键此前已按下，长按空格时只切换一次。
                    if id != TOGGLE_FOLD || msg.lParam.0 & (1 << 30) == 0 {
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
    PICTURE.with(|p| {
        p.borrow_mut().take();
    });
    result
}

/// 前台窗口属于本贴图（自身或其所属窗口）且未折叠时为选中：辉光变亮并显示工具栏。
fn update_selection(hwnd: HWND) -> Result<()> {
    let owned = unsafe {
        let foreground = GetForegroundWindow();
        !foreground.is_invalid()
            && (foreground == hwnd || GetAncestor(foreground, GA_ROOTOWNER) == hwnd)
    };
    let (changed, selected) = with_picture(|p| {
        let selected = owned && !p.folded;
        if p.selected == selected {
            return (false, selected);
        }
        p.selected = selected;
        paint_frame(&mut p.surface, selected);
        if let Some(surface) = &mut p.zoom_surface {
            paint_frame(surface, selected);
        }
        (true, selected)
    });
    bar::set_selected(selected);
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

fn save_png(owner: HWND, bytes: &[u8]) -> Result<bool> {
    use windows::Win32::UI::Controls::Dialogs::*;
    use windows::core::{PCWSTR, PWSTR};
    let mut path = [0u16; 32768];
    let name: Vec<_> = "贴图.png".encode_utf16().collect();
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

fn command(hwnd: HWND, id: u16) {
    if PICTURE.with(|p| p.borrow().as_ref().is_none_or(|p| p.exporting)) {
        return;
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
                bar::feedback(bar::COPY);
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
                    bar::feedback(bar::SAVE);
                }
            }
            bar::LESS | bar::MORE => {
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
            bar::CLOSE => unsafe {
                DestroyWindow(hwnd)?;
            },
            TOGGLE_FOLD => {
                ink::settle(hwnd)?;
                toggle_fold(hwnd)?;
            }
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
unsafe extern "system" fn procedure(hwnd: HWND, message: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        match message {
            super::pin_toolbar::COMMAND => {
                command(hwnd, wp.0 as u16);
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
                let result = if let Some(offset) = DRAG_OFFSET.get() {
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
                if !stop_drag(hwnd) {
                    report_render_error(ink::pointer_up(hwnd));
                }
                LRESULT(0)
            }
            WM_CAPTURECHANGED => {
                DRAG_OFFSET.set(None);
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
            WM_NCLBUTTONDBLCLK => {
                command(hwnd, bar::CLOSE);
                LRESULT(0)
            }
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
                command(hwnd, ink::ESCAPE);
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
            // 截图后贴图立即激活，工具栏随之出现。
            let toolbar = FindWindowW(w!("NextGesturesPinToolbar"), None).unwrap();
            ink::activate(hwnd);
            assert_eq!(GetForegroundWindow(), hwnd);
            SendMessageW(hwnd, bar::SELECTION, None, None);
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
            let press_space = |previously_down| {
                PostMessageW(
                    Some(hwnd),
                    WM_KEYDOWN,
                    WPARAM(usize::from(VK_SPACE.0)),
                    LPARAM(1 | if previously_down { 1 << 30 } else { 0 }),
                )
                .unwrap();
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
            press_space(false);
            press_space(true);
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
            press_space(false);
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
