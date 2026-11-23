//! 小型无边框透明 loading 窗口（layered window + 逐像素 alpha，软件绘制抗锯齿圆弧）。
//! 鼠标穿透、不抢焦点。

use super::anchor;
use std::cell::RefCell;
use std::f32::consts::TAU;
use std::time::Instant;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::w;

struct Spinner {
    hwnd: HWND,
    size: i32,
    pos: POINT,
    surface: super::drawing::Surface,
    start: Instant,
}

thread_local! {
    static SPINNER: RefCell<Option<Spinner>> = const { RefCell::new(None) };
}

const ACCENT: [f32; 3] = super::theme::SPINNER_ACCENT;

pub fn is_open() -> bool {
    SPINNER.with(|spinner| spinner.borrow().is_some())
}

pub fn show(anchor: RECT) {
    hide();
    let work = anchor::work_area(&anchor);
    let scale = anchor::scale_for(&anchor);
    let size = (24.0 * scale).round() as i32;
    let gap = (3.0 * scale).round() as i32;

    // 默认放在文字右上方；上方放不下就贴在右侧
    let mut x = anchor.right + gap;
    let mut y = anchor.top - size - gap;
    if y < work.top {
        y = anchor.top;
    }
    x = x.min(work.right - size).max(work.left);
    y = y.min(work.bottom - size).max(work.top);

    unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            lpszClassName: w!("NextGesturesQuickFormaterSpinner"),
            ..Default::default()
        };
        RegisterClassW(&wc);
        let Ok(hwnd) = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            w!("NextGesturesQuickFormaterSpinner"),
            w!(""),
            WS_POPUP,
            x,
            y,
            size,
            size,
            None,
            None,
            Some(hinst.into()),
            None,
        ) else {
            return;
        };

        let Some(surface) = super::drawing::Surface::new(size, size) else {
            let _ = DestroyWindow(hwnd);
            return;
        };
        let mut sp = Spinner {
            hwnd,
            size,
            pos: POINT { x, y },
            surface,
            start: Instant::now(),
        };
        sp.frame();
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        SetTimer(Some(hwnd), 1, 16, None);
        SPINNER.with(|s| *s.borrow_mut() = Some(sp));
    }
}

pub fn hide() {
    let sp = SPINNER.with(|s| s.borrow_mut().take());
    if let Some(sp) = sp {
        unsafe {
            let _ = DestroyWindow(sp.hwnd);
        }
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_TIMER {
        SPINNER.with(|s| {
            if let Ok(mut s) = s.try_borrow_mut() {
                if let Some(sp) = s.as_mut() {
                    sp.frame();
                }
            }
        });
        return LRESULT(0);
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

impl Spinner {
    fn frame(&mut self) {
        let n = self.size as usize;
        let buf = self.surface.pixels_mut();
        render(buf, n, self.start.elapsed().as_secs_f32());
        unsafe {
            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                BlendFlags: 0,
                SourceConstantAlpha: 255,
                AlphaFormat: AC_SRC_ALPHA as u8,
            };
            let size = SIZE {
                cx: self.size,
                cy: self.size,
            };
            let src = POINT::default();
            let _ = UpdateLayeredWindow(
                self.hwnd,
                None,
                Some(&self.pos),
                Some(&size),
                Some(self.surface.dc),
                Some(&src),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            );
        }
    }
}

fn cov(x: f32) -> f32 {
    x.clamp(0.0, 1.0)
}

/// 预乘 alpha 的 "over" 合成
fn over(dst: &mut [f32; 4], rgb: [f32; 3], a: f32) {
    if a <= 0.0 {
        return;
    }
    let k = 1.0 - a;
    dst[0] = rgb[0] * a + dst[0] * k;
    dst[1] = rgb[1] * a + dst[1] * k;
    dst[2] = rgb[2] * a + dst[2] * k;
    dst[3] = a + dst[3] * k;
}

/// 白色半透明圆底 + 浅色轨道 + 旋转且长度呼吸变化的蓝色圆弧（尾部渐隐）
fn render(buf: &mut [u32], n: usize, t: f32) {
    let c = n as f32 / 2.0;
    let rd = c - 0.5;
    let r = n as f32 * 0.29;
    let th = (n as f32 * 0.11).max(2.0);
    let head = t * TAU * 1.15;
    let len = (0.30 + 0.45 * (0.5 - 0.5 * (t * 2.4).cos())) * TAU;

    for py in 0..n {
        for px in 0..n {
            let dx = px as f32 + 0.5 - c;
            let dy = py as f32 + 0.5 - c;
            let d = (dx * dx + dy * dy).sqrt();
            let mut col = [0f32; 4];

            over(&mut col, [1.0, 1.0, 1.0], cov(rd - d + 0.5) * 0.92);
            over(
                &mut col,
                [0.0, 0.0, 0.0],
                cov(1.0 - (d - (rd - 0.6)).abs()) * 0.16,
            );

            let band = cov(th / 2.0 - (d - r).abs() + 0.5);
            if band > 0.0 {
                over(&mut col, ACCENT, band * 0.18);
                let ang = dy.atan2(dx);
                let rel = (head - ang).rem_euclid(TAU);
                let (inside, edge_px) = if rel <= len {
                    (true, rel.min(len - rel) * d)
                } else {
                    (false, (rel - len).min(TAU - rel) * d)
                };
                let a = if inside {
                    cov(edge_px + 0.5) * (1.0 - 0.65 * rel / len)
                } else {
                    cov(0.5 - edge_px)
                };
                over(&mut col, ACCENT, band * a);
            }

            let to8 = |v: f32| (v * 255.0).round().clamp(0.0, 255.0) as u32;
            buf[py * n + px] =
                (to8(col[3]) << 24) | (to8(col[0]) << 16) | (to8(col[1]) << 8) | to8(col[2]);
        }
    }
}
