//! 物理像素截图与逐像素阴影；每张贴图独立拥有图片及消息循环。
use anyhow::{Result, ensure};
use std::cell::RefCell;
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
    VK_MENU, VK_RETURN,
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
            width <= 16384 && height <= 16384 && width * height <= 32 * 1024 * 1024,
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
        let stride = surface.width as usize * 4;
        let bytes = surface.bytes_mut();
        let glow = if self.selected { GLOW_SELECTED } else { GLOW_IDLE };
        // 内容直接逐行复制；透明度改变时复用已合成的 DIB，不再截图或重算阴影。
        for y in 0..view.height + SHADOW * 2 {
            for x in 0..view.width + SHADOW * 2 {
                if (SHADOW..SHADOW + view.width).contains(&x)
                    && (SHADOW..SHADOW + view.height).contains(&y)
                {
                    continue;
                }
                let i = y as usize * stride + x as usize * 4;
                bytes[i..i + 4].copy_from_slice(&edge_pixel(x, y, view.width, view.height, glow));
            }
        }
        for y in 0..view.height {
            let src = ((view.y + y) as usize * self.width as usize + view.x as usize) * 4;
            let dst = (y + SHADOW) as usize * stride + SHADOW as usize * 4;
            bytes[dst..dst + view.width as usize * 4]
                .copy_from_slice(&self.pixels[src..src + view.width as usize * 4]);
        }
        Ok(surface)
    }
}

fn shadow_alpha(x: i32, y: i32, width: i32, height: i32) -> u8 {
    let dx = (SHADOW - x).max(x - (SHADOW + width - 1)).max(0) as f32;
    // 阴影向下偏移 3px，在预留的 20px 中平滑衰减至透明。
    let dy = (SHADOW + 3 - y).max(y - (SHADOW + height + 2)).max(0) as f32;
    let distance = (dx * dx + dy * dy).sqrt();
    (95.0 * (-distance * distance / 85.0).exp() * (1.0 - distance / 20.0).max(0.0)).round() as u8
}

/// 选中时辉光更亮，未选中（失活）时更淡。
const GLOW_SELECTED: f32 = 1.8;
const GLOW_IDLE: f32 = 0.5;

/// 淡蓝细边与外扩辉光叠在黑色阴影上，输出预乘 BGRA；不覆盖截图内容。
fn edge_pixel(x: i32, y: i32, width: i32, height: i32, strength: f32) -> [u8; 4] {
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
        (235 * glow / 255) as u8,
        (152 * glow / 255) as u8,
        (52 * glow / 255) as u8,
        (glow + shadow * (255 - glow) / 255) as u8,
    ]
}

thread_local! { static PICTURE: RefCell<Option<Picture>> = const { RefCell::new(None) }; }
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
        (
            p.surface.dc,
            SIZE {
                cx: p.surface.width,
                cy: p.surface.height,
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

fn toggle_fold(hwnd: HWND, click: POINT) -> Result<()> {
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
                x: (click.x - rect.left - SHADOW - width / 2).clamp(0, p.width - width),
                y: (click.y - rect.top - SHADOW - height / 2).clamp(0, p.height - height),
                width,
                height,
            }
        };
        let surface = p.compose(view)?;
        let position = POINT {
            x: rect.left + view.x - p.view.x,
            y: rect.top + view.y - p.view.y,
        };
        p.surface = surface;
        p.view = view;
        p.folded = !p.folded;
        Ok(position)
    })?;
    render(hwnd, Some(position))?;
    // 折叠后的小图视为失活；展开后若仍在前台则重新选中，工具栏跟随新位置。
    update_selection(hwnd)
}

fn hit_test(hwnd: HWND, point: POINT) -> LRESULT {
    let mut rect = RECT::default();
    if unsafe { GetWindowRect(hwnd, &mut rect) }.is_err() {
        return LRESULT(HTTRANSPARENT as isize);
    }
    let (width, height, drawing) = PICTURE.with(|p| {
        let p = p.borrow();
        let p = p.as_ref().unwrap();
        (p.view.width, p.view.height, p.ink.tool.is_some() && !p.folded)
    });
    if (rect.left + SHADOW..rect.left + SHADOW + width).contains(&point.x)
        && (rect.top + SHADOW..rect.top + SHADOW + height).contains(&point.y)
    {
        // 涂鸦模式下内容区作为客户区接收鼠标，否则整张图可拖动。
        LRESULT(if drawing { HTCLIENT } else { HTCAPTION } as isize)
    } else {
        LRESULT(HTTRANSPARENT as isize)
    }
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
                w!("贴图 · 拖动移动 / 滚轮透明度 / 双击折叠 / 右键关闭"),
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
                    command(hwnd, id);
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
    let (changed, selected) = with_picture(|p| -> Result<(bool, bool)> {
        let selected = owned && !p.folded;
        if p.selected == selected {
            return Ok((false, selected));
        }
        p.selected = selected;
        p.surface = p.compose(p.view)?;
        p.paint_view();
        Ok((true, selected))
    })?;
    bar::show(selected);
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
                report_render_error(ink::pointer_move(hwnd, point(lp), wp.0 & 0x0004 != 0));
                LRESULT(0)
            }
            WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
                report_render_error(ink::pointer_down(hwnd, point(lp)));
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                report_render_error(ink::pointer_up(hwnd));
                LRESULT(0)
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
            WM_WINDOWPOSCHANGED => {
                if let Err(e) = super::pin_toolbar::position() {
                    log::warn!("贴图工具栏定位失败: {e}");
                }
                DefWindowProcW(hwnd, message, wp, lp)
            }
            WM_NCHITTEST => hit_test(hwnd, point(lp)),
            // HTCAPTION 左键交给 DefWindowProc 的系统移动循环。
            WM_NCLBUTTONDBLCLK => {
                report_render_error(toggle_fold(hwnd, point(lp)));
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
                // 涂鸦模式下调粗细 / 字号，否则（含失活贴图）调透明度。
                match ink::wheel(hwnd, ((wp.0 >> 16) as u16) as i16 as i32) {
                    Ok(true) => return LRESULT(0),
                    Ok(false) => {}
                    Err(e) => {
                        report_render_error(Err(e));
                        return LRESULT(0);
                    }
                }
                let changed = PICTURE.with(|p| {
                    let mut p = p.borrow_mut();
                    let p = p.as_mut().unwrap();
                    p.wheel_remainder += ((wp.0 >> 16) as u16) as i16 as i32;
                    let steps = p.wheel_remainder / 120;
                    p.wheel_remainder %= 120;
                    let alpha = (i32::from(p.alpha) + steps * 5).clamp(50, 255) as u8;
                    let changed = alpha != p.alpha;
                    p.alpha = alpha;
                    changed
                });
                if changed {
                    report_render_error(render(hwnd, None));
                    let alpha = PICTURE.with(|p| p.borrow().as_ref().unwrap().alpha);
                    super::pin_toolbar::alpha(alpha);
                }
                LRESULT(0)
            }
            WM_DESTROY => {
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
        assert_eq!(edge_pixel(0, 0, 200, 100, 1.0), [0; 4]);
        let edge = edge_pixel(19, 70, 200, 100, 1.0);
        assert!(edge[0] > edge[1] && edge[1] > edge[2]);
        assert!(edge[..3].iter().all(|&channel| channel <= edge[3]));
        assert!(edge[0] > edge_pixel(12, 70, 200, 100, 1.0)[0]);
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
    fn desktop_pin_toolbar_fold_opacity_save_cancel_and_close() {
        use std::time::{Duration, Instant};
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
            let shadow_pixel = gray(99, 180);
            // 截图后贴图立即激活，工具栏随之出现。
            let toolbar = FindWindowW(w!("NextGesturesPinToolbar"), None).unwrap();
            let _ = SetForegroundWindow(hwnd);
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
            SendMessageW(
                hwnd,
                WM_NCLBUTTONDBLCLK,
                Some(WPARAM(HTCAPTION as usize)),
                Some(packed(200, 160)),
            );
            let mut folded = RECT::default();
            GetWindowRect(hwnd, &mut folded).unwrap();
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
            SendMessageW(
                hwnd,
                WM_NCLBUTTONDBLCLK,
                Some(WPARAM(HTCAPTION as usize)),
                Some(packed(folded.left + 60, folded.top + 70)),
            );
            let mut restored = RECT::default();
            GetWindowRect(hwnd, &mut restored).unwrap();
            SendMessageW(
                hwnd,
                WM_MOUSEWHEEL,
                Some(WPARAM((-120i16 as u16 as usize) << 16)),
                None,
            );
            let pixel = await_gray(170, 180, 5);
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
            if cancelled {
                let deadline = Instant::now() + Duration::from_secs(3);
                while !IsWindowVisible(toolbar).as_bool() && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(20));
                }
                SendMessageW(
                    GetDlgItem(Some(toolbar), i32::from(super::super::pin_toolbar::CLOSE)).unwrap(),
                    BM_CLICK,
                    None,
                    None,
                );
            } else {
                SendMessageW(hwnd, WM_CLOSE, None, None);
            }
            let deadline = Instant::now() + Duration::from_secs(2);
            while IsWindow(Some(hwnd)).as_bool() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            let toolbar_closed = !IsWindow(Some(hwnd)).as_bool();
            if !toolbar_closed {
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
                toolbar_closed,
                toolbar_native,
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
            (evidence.7 & 255).abs_diff(5) <= 2,
            "wheel opacity pixel: {:#x}",
            evidence.7
        );
        assert!(
            (evidence.8 & 255) <= 2,
            "initial opacity pixel: {:#x}",
            evidence.8
        );
        assert!(
            (100..250).contains(&(evidence.9 & 255)),
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
        assert!(evidence.16, "toolbar close command did not close the pin");
        assert!(
            evidence.17,
            "toolbar controls did not bypass the gesture hook"
        );
        println!(
            "Desktop pin: toolbar visible, opacity buttons update pixels, original fold/restore, save cancellation, toolbar close released resources"
        );
    }
}
