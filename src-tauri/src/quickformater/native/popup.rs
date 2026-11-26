//! 悬浮结果窗：显示翻译（流式增长）、提示信息或错误。
//! - 不抢焦点（WS_EX_NOACTIVATE），Esc / 点击外部 / 目标窗口切换时关闭
//! - 位置像右键菜单一样自适应：优先放在选区上方，其次下方、右侧、左侧，始终不超出显示器工作区
//! - 内容超过可用高度时可滚轮滚动，流式输出时自动跟随到底部

use super::WM_APP_WHEEL;
use super::anchor;
use super::drawing;
use super::theme;
use super::utf16;
use std::cell::RefCell;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::w;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// AI 动作结果（气泡显示，或替换失败时改为显示），标题为动作名
    Result,
    /// 已替换原文后显示的说明（动作开启了「显示说明」）
    Tutor,
    Info,
    Error,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Side {
    Above,
    Below,
    Right,
    Left,
    /// 四周都放不下时退化为靠近鼠标，不可避免地会遮挡部分文字
    Overlay,
}

const WM_MOUSELEAVE: u32 = 0x02A3;
const TIMER_FG: usize = 1;
const TIMER_AUTOCLOSE: usize = 2;

struct Popup {
    hwnd: HWND,
    kind: Kind,
    /// 自定义标题（动作名），空串时用 Kind 的默认标题
    title: String,
    anchor: RECT,
    work: RECT,
    scale: f32,
    side: Option<Side>,
    text: String,
    estimate: Option<String>,
    streaming: bool,
    failed: bool,
    font: HFONT,
    small: HFONT,
    title_font: HFONT,
    line_h: i32,
    text_w: i32,
    content_h: i32,
    view_h: i32,
    scroll: i32,
    follow: bool,
    w: i32,
    h: i32,
    copy_rect: RECT,
    close_rect: RECT,
    hover: bool,
    close_hover: bool,
    tracking: bool,
    copied: bool,
    fg: HWND,
}

thread_local! {
    static POPUP: RefCell<Option<Popup>> = const { RefCell::new(None) };
}

pub struct Options<'a> {
    pub kind: Kind,
    pub title: &'a str,
    pub text: &'a str,
    /// 用于预估最终尺寸（翻译模式传原文），减少流式过程中窗口跳动
    pub estimate: Option<&'a str>,
    pub streaming: bool,
    pub font_size: f32,
    pub auto_close_ms: u32,
}

pub fn is_open() -> bool {
    POPUP.with(|p| p.borrow().is_some())
}

fn make_font(pt: f32, scale: f32, weight: i32) -> HFONT {
    let mut lf = LOGFONTW {
        lfHeight: -(pt * 96.0 * scale / 72.0).round() as i32,
        lfWeight: weight,
        lfQuality: CLEARTYPE_QUALITY,
        ..Default::default()
    };
    let face = utf16("Microsoft YaHei UI");
    lf.lfFaceName[..face.len()].copy_from_slice(&face);
    unsafe { CreateFontIndirectW(&lf) }
}

pub fn open(anchor: RECT, o: Options) {
    close();
    let work = anchor::work_area(&anchor);
    let scale = anchor::scale_for(&anchor);
    unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        let wc = WNDCLASSW {
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            lpszClassName: w!("NextGesturesQuickFormaterPopup"),
            ..Default::default()
        };
        RegisterClassW(&wc);
        let Ok(hwnd) = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            w!("NextGesturesQuickFormaterPopup"),
            w!("QuickFormater"),
            WS_POPUP,
            0,
            0,
            1,
            1,
            None,
            None,
            Some(hinst.into()),
            None,
        ) else {
            return;
        };
        let font = make_font(o.font_size, scale, 400);
        let small = make_font((o.font_size - 1.5).max(8.0), scale, 400);
        let title_font = make_font(9.0, scale, 600);
        let mut p = Popup {
            hwnd,
            kind: o.kind,
            title: o.title.to_owned(),
            anchor,
            work,
            scale,
            side: None,
            text: normalize(o.text),
            estimate: o.estimate.map(normalize),
            streaming: o.streaming,
            failed: o.kind == Kind::Error,
            font,
            small,
            title_font,
            line_h: 0,
            text_w: 0,
            content_h: 0,
            view_h: 0,
            scroll: 0,
            follow: true,
            w: 0,
            h: 0,
            copy_rect: RECT::default(),
            close_rect: RECT::default(),
            hover: false,
            close_hover: false,
            tracking: false,
            copied: false,
            fg: GetForegroundWindow(),
        };
        p.relayout();
        POPUP.with(|s| *s.borrow_mut() = Some(p));

        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        super::hooks::set_popup(hwnd, (theme::SHADOW_PAD * scale).round() as i32);
        SetTimer(Some(hwnd), TIMER_FG, 250, None);
        if o.auto_close_ms > 0 {
            SetTimer(Some(hwnd), TIMER_AUTOCLOSE, o.auto_close_ms, None);
        }
    }
}

pub fn close() {
    let hwnd = POPUP.with(|p| p.borrow().as_ref().map(|p| p.hwnd));
    if let Some(hwnd) = hwnd {
        super::hooks::set_popup(HWND::default(), 0);
        unsafe {
            let _ = DestroyWindow(hwnd);
        }
    }
}

fn with_popup(f: impl FnOnce(&mut Popup)) {
    POPUP.with(|p| {
        if let Ok(mut b) = p.try_borrow_mut() {
            if let Some(p) = b.as_mut() {
                f(p);
            }
        }
    });
}

pub fn append(s: &str) {
    with_popup(|p| {
        p.text.push_str(&normalize(s));
        p.relayout();
    });
}

pub fn finish(text: &str) {
    with_popup(|p| {
        p.text = normalize(text);
        p.streaming = false;
        if p.text.trim().is_empty() {
            p.text = "AI 返回了空内容".into();
            p.failed = true;
        }
        p.relayout();
    });
}

pub fn fail(msg: &str) {
    with_popup(|p| {
        p.streaming = false;
        p.failed = true;
        if p.text.trim().is_empty() {
            p.text = msg.to_string();
            p.kind = Kind::Error;
        } else {
            p.text.push_str("\n\n⚠ ");
            p.text.push_str(msg);
        }
        p.relayout();
    });
}

fn normalize(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\r', "\n")
}

unsafe fn rounded_fill(hdc: HDC, r: RECT, radius: i32, color: COLORREF) {
    drawing::rounded_fill(hdc, r, radius as f32, color);
}

unsafe fn label(hdc: HDC, text: &str, r: &mut RECT, color: COLORREF, align: DRAW_TEXT_FORMAT) {
    SetTextColor(hdc, color);
    draw_text(
        hdc,
        &mut utf16(text),
        r,
        DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX | DT_END_ELLIPSIS | align,
    );
}

const DT_TEXT: DRAW_TEXT_FORMAT =
    DRAW_TEXT_FORMAT(DT_WORDBREAK.0 | DT_NOPREFIX.0 | DT_EDITCONTROL.0 | DT_EXPANDTABS.0);

/// DrawTextW 的空串保护：windows-rs 对空切片会传入悬空指针，GDI 会因此崩溃
unsafe fn draw_text(hdc: HDC, buf: &mut [u16], r: &mut RECT, fmt: DRAW_TEXT_FORMAT) -> i32 {
    if buf.is_empty() {
        if fmt.contains(DT_CALCRECT) {
            r.right = r.left;
            r.bottom = r.top;
        }
        return 0;
    }
    DrawTextW(hdc, buf, r, fmt)
}

unsafe fn measure_natural(hdc: HDC, s: &str) -> i32 {
    let mut buf = utf16(s);
    let mut r = RECT::default();
    draw_text(
        hdc,
        &mut buf,
        &mut r,
        DT_CALCRECT | DT_NOPREFIX | DT_EXPANDTABS,
    );
    r.right
}

unsafe fn measure_height(hdc: HDC, s: &str, width: i32) -> i32 {
    let mut buf = utf16(s);
    let mut r = RECT {
        left: 0,
        top: 0,
        right: width,
        bottom: 0,
    };
    draw_text(hdc, &mut buf, &mut r, DT_TEXT | DT_CALCRECT);
    r.bottom
}

/// 选择摆放方向并计算窗口矩形。
/// `sticky` 为上次的方向：只要还放得下就不换，避免流式增长时窗口来回跳。
pub fn place(
    anchor: RECT,
    work: RECT,
    w: i32,
    h: i32,
    gap: i32,
    sticky: Option<Side>,
    min_h: i32,
    cursor: POINT,
) -> (Side, RECT) {
    let ww = work.right - work.left;
    let wh = work.bottom - work.top;
    let avail = |s: Side| -> (i32, i32) {
        match s {
            Side::Above => (ww, anchor.top - gap - work.top),
            Side::Below => (ww, work.bottom - anchor.bottom - gap),
            Side::Right => (work.right - anchor.right - gap, wh),
            Side::Left => (anchor.left - gap - work.left, wh),
            Side::Overlay => (ww, wh),
        }
    };
    let fits = |s: Side| {
        let (aw, ah) = avail(s);
        aw >= w && ah >= h
    };
    let order = [Side::Above, Side::Below, Side::Right, Side::Left];
    let min_w = w.min(ww / 3);

    let side = if let Some(s) = sticky.filter(|s| fits(*s)) {
        s
    } else if let Some(s) = order.into_iter().find(|s| fits(*s)) {
        s
    } else {
        // 都放不下完整内容：保持原方向（若仍有合理空间），否则选可见面积最大的方向（内容可滚动）
        let usable = |s: Side| {
            let (aw, ah) = avail(s);
            aw >= min_w && ah >= min_h
        };
        let area = |s: Side| {
            let (aw, ah) = avail(s);
            aw.min(w).max(0) as i64 * ah.min(h).max(0) as i64
        };
        match sticky.filter(|s| *s != Side::Overlay && usable(*s)) {
            Some(s) => s,
            None => order
                .into_iter()
                .filter(|s| usable(*s))
                .max_by_key(|s| area(*s))
                .unwrap_or(Side::Overlay),
        }
    };

    let (aw, ah) = avail(side);
    let rw = w.min(aw).max(1);
    let rh = h.min(ah).max(1);
    let clamp_x = |x: i32| x.min(work.right - rw).max(work.left);
    let clamp_y = |y: i32| y.min(work.bottom - rh).max(work.top);
    let (x, y) = match side {
        Side::Above => (clamp_x(anchor.left), anchor.top - gap - rh),
        Side::Below => (clamp_x(anchor.left), anchor.bottom + gap),
        Side::Right => (anchor.right + gap, clamp_y(anchor.top)),
        Side::Left => (anchor.left - gap - rw, clamp_y(anchor.top)),
        Side::Overlay => (clamp_x(cursor.x + gap * 2), clamp_y(cursor.y + gap * 3)),
    };
    (
        side,
        RECT {
            left: x,
            top: y,
            right: x + rw,
            bottom: y + rh,
        },
    )
}

impl Popup {
    fn s(&self, v: f32) -> i32 {
        (v * self.scale).round() as i32
    }

    fn chrome(&self) -> (i32, i32, i32, i32) {
        // (pad, footer_gap, footer_h, bottom_pad)
        (
            self.s(theme::PAD),
            self.s(theme::FOOTER_GAP),
            self.s(theme::BUTTON_H),
            self.s(theme::BOTTOM_PAD),
        )
    }

    fn relayout(&mut self) {
        let (pad, fgap, footer_h, bpad) = self.chrome();
        let body_top = self.s(theme::HEADER_H);
        let chrome_v = body_top + fgap + footer_h + bpad;
        let work_w = self.work.right - self.work.left;
        let work_h = self.work.bottom - self.work.top;
        let max_text_w = self
            .s(theme::MAX_TEXT_W)
            .min(work_w - 2 * pad - self.s(16.0))
            .max(1);
        let min_text_w = self.s(theme::MIN_TEXT_W).min(max_text_w);
        let max_h = (work_h as f32 * 0.6) as i32;

        unsafe {
            let hdc = GetDC(Some(self.hwnd));
            let old = SelectObject(hdc, self.font.into());
            self.line_h = measure_height(hdc, "国", 1000).max(1);

            let mut natural = measure_natural(hdc, &self.text);
            if let Some(e) = &self.estimate {
                natural = natural.max(measure_natural(hdc, e));
            }
            self.text_w = natural.clamp(min_text_w, max_text_w);
            self.content_h = if self.text.is_empty() {
                self.line_h
            } else {
                measure_height(hdc, &self.text, self.text_w).max(self.line_h)
            };
            let est_h = self
                .estimate
                .as_ref()
                .map(|e| measure_height(hdc, e, self.text_w))
                .unwrap_or(0);

            SelectObject(hdc, self.small.into());
            let label = utf16(self.copy_label());
            let mut lr = RECT::default();
            let mut lb = label.clone();
            draw_text(hdc, &mut lb, &mut lr, DT_CALCRECT | DT_SINGLELINE);
            let w = self.text_w + 2 * pad;
            let h = (self.content_h + chrome_v).min(max_h);
            let gap = self.s(theme::ANCHOR_GAP);
            let min_h = self.line_h * 2 + chrome_v;
            let cursor = anchor::cursor_pos();

            if self.side.is_none() {
                // 首次摆放按预估的最终高度选方向
                let h_est = (self.content_h.max(est_h) + chrome_v).min(max_h);
                self.side =
                    Some(place(self.anchor, self.work, w, h_est, gap, None, min_h, cursor).0);
            }
            let (mut side, mut r) =
                place(self.anchor, self.work, w, h, gap, self.side, min_h, cursor);
            // 极窄侧边容不下标题和按钮，退回屏幕内完整浮窗，避免热区互相覆盖。
            if r.right - r.left < min_text_w + 2 * pad {
                (side, r) = place(
                    self.anchor,
                    self.work,
                    w,
                    h,
                    gap,
                    Some(Side::Overlay),
                    min_h,
                    cursor,
                );
            }
            // 侧边空间不足时窗口会收窄；按实际正文宽度重新测量，防止长文被右侧裁掉。
            SelectObject(hdc, self.font.into());
            self.text_w = (r.right - r.left - 2 * pad).max(1);
            self.content_h = measure_height(hdc, &self.text, self.text_w).max(self.line_h);
            let actual_h = (self.content_h + chrome_v).min(max_h);
            let (side, measured) = place(
                self.anchor,
                self.work,
                r.right - r.left,
                actual_h,
                gap,
                Some(side),
                min_h,
                cursor,
            );
            r = measured;
            SelectObject(hdc, old);
            ReleaseDC(Some(self.hwnd), hdc);
            self.side = Some(side);
            self.w = r.right - r.left;
            self.h = r.bottom - r.top;
            self.view_h = (self.h - chrome_v).max(1);

            let max_scroll = (self.content_h - self.view_h).max(0);
            self.scroll = if self.follow && self.streaming {
                max_scroll
            } else {
                self.scroll.clamp(0, max_scroll)
            };

            let btn_w = ((lr.right - lr.left) + self.s(42.0)).max(self.s(76.0));
            let fy = self.h - bpad - footer_h;
            self.copy_rect = RECT {
                left: self.w - pad - btn_w,
                top: fy,
                right: self.w - pad,
                bottom: fy + footer_h,
            };
            self.close_rect = RECT {
                left: self.w - pad - self.s(24.0),
                top: self.s(14.0),
                right: self.w - pad,
                bottom: self.s(38.0),
            };

            let inset = self.s(theme::SHADOW_PAD);
            let _ = SetWindowPos(
                self.hwnd,
                Some(HWND_TOPMOST),
                r.left - inset,
                r.top - inset,
                self.w + 2 * inset,
                self.h + 2 * inset,
                SWP_NOACTIVATE,
            );
            self.present();
        }
    }

    fn copy_label(&self) -> &'static str {
        if self.copied {
            "已复制 ✓"
        } else {
            "复制"
        }
    }

    fn status_label(&self) -> &'static str {
        match (self.kind, self.streaming, self.failed) {
            (_, true, _) => "正在处理 · Esc 取消",
            (Kind::Error, _, _) | (_, _, true) => "出错了 · Esc 关闭",
            (Kind::Result, _, _) => "可复制结果 · Esc 关闭",
            (Kind::Tutor, _, _) => "已替换原文 · Esc 关闭",
            _ => "Esc 关闭",
        }
    }

    fn title(&self) -> &str {
        match self.kind {
            Kind::Result if !self.title.is_empty() => &self.title,
            Kind::Result => "AI 结果",
            Kind::Tutor => "修改說明",
            Kind::Info => "提示",
            Kind::Error => "遇到问题",
        }
    }

    fn can_copy(&self) -> bool {
        !self.streaming && !self.text.trim().is_empty()
    }

    fn client_point(&self, pt: POINT) -> POINT {
        let inset = self.s(theme::SHADOW_PAD);
        POINT {
            x: pt.x - inset,
            y: pt.y - inset,
        }
    }

    fn frame(&self) -> Option<drawing::Surface> {
        unsafe {
            let card = drawing::Surface::new(self.w, self.h)?;
            self.paint(card.dc);
            // 在读取 GDI 写入的 DIB 像素之前等待该线程的批处理完成。
            let _ = GdiFlush();
            let inset = self.s(theme::SHADOW_PAD);
            let mut output = drawing::Surface::new(self.w + 2 * inset, self.h + 2 * inset)?;
            drawing::compose(
                &card,
                &mut output,
                inset,
                theme::WINDOW_RADIUS * self.scale,
                theme::BORDER,
                self.scale,
            );
            Some(output)
        }
    }

    fn present(&self) {
        unsafe {
            let Some(output) = self.frame() else {
                return;
            };
            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                BlendFlags: 0,
                SourceConstantAlpha: 255,
                AlphaFormat: AC_SRC_ALPHA as u8,
            };
            let size = SIZE {
                cx: output.width,
                cy: output.height,
            };
            let origin = POINT::default();
            if let Err(e) = UpdateLayeredWindow(
                self.hwnd,
                None,
                None,
                Some(&size),
                Some(output.dc),
                Some(&origin),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            ) {
                log::debug!("popup composition failed: {e}");
            }
        }
    }

    unsafe fn paint(&self, hdc: HDC) {
        let (pad, _fgap, footer_h, bpad) = self.chrome();
        let body_top = self.s(theme::HEADER_H);
        let mem = CreateCompatibleDC(Some(hdc));
        let bmp = CreateCompatibleBitmap(hdc, self.w, self.h);
        let old_bmp = SelectObject(mem, bmp.into());
        let full = RECT {
            left: 0,
            top: 0,
            right: self.w,
            bottom: self.h,
        };
        let bg = CreateSolidBrush(theme::SURFACE);
        FillRect(mem, &full, bg);
        let _ = DeleteObject(bg.into());
        // 表面先作为不透明内容绘制，present 再为圆角和阴影生成覆盖率 alpha。
        SetBkMode(mem, TRANSPARENT);
        let old_font = SelectObject(mem, self.title_font.into());

        // 标题：淡蓝标识、半粗标题、弱化品牌文字与独立关闭热区。
        let badge = RECT {
            left: pad,
            top: self.s(14.0),
            right: pad + self.s(26.0),
            bottom: self.s(40.0),
        };
        rounded_fill(mem, badge, self.s(7.0), theme::ACCENT_WASH);
        let mut icon_rect = badge;
        label(mem, "A", &mut icon_rect, theme::ACCENT, DT_CENTER);
        let mut title_rect = RECT {
            left: badge.right + self.s(9.0),
            top: badge.top,
            right: self.close_rect.left - self.s(94.0),
            bottom: badge.bottom,
        };
        label(mem, self.title(), &mut title_rect, theme::TEXT, DT_LEFT);
        SelectObject(mem, self.small.into());
        let mut brand_rect = RECT {
            left: title_rect.right,
            top: badge.top,
            right: self.close_rect.left - self.s(8.0),
            bottom: badge.bottom,
        };
        label(
            mem,
            "QuickFormater",
            &mut brand_rect,
            theme::MUTED,
            DT_RIGHT,
        );
        if self.close_hover {
            rounded_fill(mem, self.close_rect, self.s(7.0), theme::NEUTRAL_HOVER);
        }
        let cx = (self.close_rect.left + self.close_rect.right) / 2;
        let cy = (self.close_rect.top + self.close_rect.bottom) / 2;
        let d = self.s(3.0).max(2);
        drawing::line(
            mem,
            POINT {
                x: cx - d,
                y: cy - d,
            },
            POINT {
                x: cx + d,
                y: cy + d,
            },
            self.scale,
            theme::MUTED,
        );
        drawing::line(
            mem,
            POINT {
                x: cx + d,
                y: cy - d,
            },
            POINT {
                x: cx - d,
                y: cy + d,
            },
            self.scale,
            theme::MUTED,
        );

        // 正文：仅裁切正文区域，滚动不影响标题和底部操作。
        SelectObject(mem, self.font.into());
        let view = RECT {
            left: pad,
            top: body_top,
            right: pad + self.text_w,
            bottom: body_top + self.view_h,
        };
        IntersectClipRect(mem, view.left, view.top, view.right, view.bottom);
        SetTextColor(mem, theme::TEXT);
        let mut tr = RECT {
            left: pad,
            top: body_top - self.scroll,
            right: pad + self.text_w,
            bottom: body_top - self.scroll + self.content_h,
        };
        let display = if self.text.is_empty() && self.streaming {
            "正在生成内容…"
        } else {
            &self.text
        };
        let mut buf = utf16(display);
        draw_text(mem, &mut buf, &mut tr, DT_TEXT);
        SelectClipRgn(mem, None);

        if self.content_h > self.view_h {
            let track_h = self.view_h;
            let thumb_h = ((track_h as f32 * self.view_h as f32 / self.content_h as f32) as i32)
                .max(self.s(16.0))
                .min(track_h);
            let max_scroll = self.content_h - self.view_h;
            let thumb_y = body_top
                + ((track_h - thumb_h) as f32 * self.scroll as f32 / max_scroll as f32) as i32;
            let x = self.w - self.s(7.0);
            rounded_fill(
                mem,
                RECT {
                    left: x,
                    top: thumb_y,
                    right: x + self.s(3.0),
                    bottom: thumb_y + thumb_h,
                },
                self.s(2.0),
                theme::SCROLL,
            );
        }

        // 无分隔线底栏：状态圆点、灰色短提示与蓝色胶囊按钮。
        SelectObject(mem, self.small.into());
        let fy = self.h - bpad - footer_h;
        let status_color = if self.failed {
            theme::ERROR
        } else {
            theme::ACCENT
        };
        let dot_y = fy + footer_h / 2 - self.s(2.0);
        rounded_fill(
            mem,
            RECT {
                left: pad,
                top: dot_y,
                right: pad + self.s(5.0),
                bottom: dot_y + self.s(5.0),
            },
            self.s(3.0),
            status_color,
        );
        let mut sr = RECT {
            left: pad + self.s(11.0),
            top: fy,
            right: self.copy_rect.left - self.s(10.0),
            bottom: fy + footer_h,
        };
        label(mem, self.status_label(), &mut sr, theme::MUTED, DT_LEFT);

        let enabled = self.can_copy();
        let fill = if !enabled {
            theme::ACCENT_WASH
        } else if self.hover {
            theme::ACCENT_HOVER
        } else {
            theme::ACCENT
        };
        let ink = if enabled {
            theme::SURFACE
        } else {
            theme::MUTED
        };
        rounded_fill(mem, self.copy_rect, footer_h / 2, fill);
        // 用两个线框绘制复制图标，保持图标在不同系统上的外观一致。
        let ix = self.copy_rect.left + self.s(13.0);
        let iy = self.copy_rect.top + (footer_h - self.s(12.0)) / 2;
        drawing::rounded_stroke(
            mem,
            RECT {
                left: ix,
                top: iy,
                right: ix + self.s(8.0),
                bottom: iy + self.s(9.0),
            },
            self.scale,
            self.scale,
            ink,
        );
        drawing::rounded_stroke(
            mem,
            RECT {
                left: ix + self.s(3.0),
                top: iy + self.s(3.0),
                right: ix + self.s(11.0),
                bottom: iy + self.s(12.0),
            },
            self.scale,
            self.scale,
            ink,
        );
        let mut cr = RECT {
            left: ix + self.s(16.0),
            right: self.copy_rect.right - self.s(10.0),
            ..self.copy_rect
        };
        label(mem, self.copy_label(), &mut cr, ink, DT_CENTER);

        SelectObject(mem, old_font);
        let _ = BitBlt(hdc, 0, 0, self.w, self.h, Some(mem), 0, 0, SRCCOPY);
        SelectObject(mem, old_bmp);
        let _ = DeleteObject(bmp.into());
        let _ = DeleteDC(mem);
    }

    fn scroll_by(&mut self, delta: i32) {
        let max_scroll = (self.content_h - self.view_h).max(0);
        self.scroll = (self.scroll - delta * self.line_h * 3 / 120).clamp(0, max_scroll);
        self.follow = self.scroll >= max_scroll;
        self.present();
    }

    fn click(&mut self, pt: POINT) {
        if !self.can_copy() || !unsafe { PtInRect(&self.copy_rect, pt) }.as_bool() {
            return;
        }
        match super::super::selection::copy_result(self.hwnd.0 as usize, self.text.trim()) {
            Ok(()) => self.copied = true,
            Err(e) => log::debug!("copy failed: {e}"),
        }
        self.relayout();
    }
}

fn lparam_point(lparam: LPARAM) -> POINT {
    POINT {
        x: (lparam.0 & 0xFFFF) as i16 as i32,
        y: ((lparam.0 >> 16) & 0xFFFF) as i16 as i32,
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_MOUSEACTIVATE => return LRESULT(MA_NOACTIVATE as isize),
        WM_ERASEBKGND => return LRESULT(1),
        WM_PRINT | WM_PRINTCLIENT => {
            POPUP.with(|p| {
                if let Ok(b) = p.try_borrow() {
                    if let Some(frame) = b.as_ref().and_then(Popup::frame) {
                        let dest = HDC(wparam.0 as *mut _);
                        let _ = BitBlt(
                            dest,
                            0,
                            0,
                            frame.width,
                            frame.height,
                            Some(frame.dc),
                            0,
                            0,
                            SRCCOPY,
                        );
                    }
                }
            });
            return LRESULT(0);
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let _ = BeginPaint(hwnd, &mut ps);
            POPUP.with(|p| {
                if let Ok(b) = p.try_borrow() {
                    if let Some(p) = b.as_ref() {
                        p.present();
                    }
                }
            });
            let _ = EndPaint(hwnd, &ps);
            return LRESULT(0);
        }
        WM_MOUSEMOVE => {
            let pt = lparam_point(lparam);
            with_popup(|p| {
                let pt = p.client_point(pt);
                if !p.tracking {
                    let mut tme = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    p.tracking = TrackMouseEvent(&mut tme).is_ok();
                }
                let hover = p.can_copy() && PtInRect(&p.copy_rect, pt).as_bool();
                let close_hover = PtInRect(&p.close_rect, pt).as_bool();
                if hover != p.hover || close_hover != p.close_hover {
                    p.hover = hover;
                    p.close_hover = close_hover;
                    p.present();
                }
            });
            return LRESULT(0);
        }
        WM_MOUSELEAVE => {
            with_popup(|p| {
                p.tracking = false;
                if p.hover || p.close_hover {
                    p.hover = false;
                    p.close_hover = false;
                    p.present();
                }
            });
            return LRESULT(0);
        }
        WM_SETCURSOR => {
            let mut hover = false;
            with_popup(|p| hover = p.hover || p.close_hover);
            if hover {
                SetCursor(LoadCursorW(None, IDC_HAND).ok());
                return LRESULT(1);
            }
        }
        WM_LBUTTONDOWN => {
            let pt = lparam_point(lparam);
            let mut dismiss = false;
            with_popup(|p| {
                let pt = p.client_point(pt);
                dismiss = PtInRect(&p.close_rect, pt).as_bool();
                if !dismiss {
                    p.click(pt);
                }
            });
            // 借用释放后再销毁；WM_NCDESTROY 需要取出 Popup 并释放字体。
            if dismiss {
                close();
            }
            return LRESULT(0);
        }
        WM_APP_WHEEL => {
            let delta = wparam.0 as isize as i32;
            with_popup(|p| p.scroll_by(delta));
            return LRESULT(0);
        }
        WM_TIMER => {
            let mut close_it = wparam.0 == TIMER_AUTOCLOSE;
            if wparam.0 == TIMER_FG {
                // 用户切换到别的窗口（Alt+Tab、任务栏等）时自动关闭
                with_popup(|p| close_it = GetForegroundWindow() != p.fg);
            }
            if close_it {
                super::hooks::set_popup(HWND::default(), 0);
                let _ = DestroyWindow(hwnd);
            }
            return LRESULT(0);
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            return LRESULT(0);
        }
        WM_NCDESTROY => {
            let p = POPUP.with(|p| p.try_borrow_mut().ok().and_then(|mut b| b.take()));
            if let Some(p) = p {
                let _ = DeleteObject(p.font.into());
                let _ = DeleteObject(p.small.into());
                let _ = DeleteObject(p.title_font.into());
            }
        }
        _ => {}
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORK: RECT = RECT {
        left: 0,
        top: 0,
        right: 1920,
        bottom: 1040,
    };
    const CUR: POINT = POINT { x: 500, y: 500 };

    fn r(l: i32, t: i32, rr: i32, b: i32) -> RECT {
        RECT {
            left: l,
            top: t,
            right: rr,
            bottom: b,
        }
    }

    fn inside(a: &RECT, w: &RECT) -> bool {
        a.left >= w.left && a.top >= w.top && a.right <= w.right && a.bottom <= w.bottom
    }

    fn overlaps(a: &RECT, b: &RECT) -> bool {
        a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom
    }

    #[test]
    fn prefers_above() {
        let a = r(100, 500, 400, 520);
        let (s, p) = place(a, WORK, 300, 100, 6, None, 60, CUR);
        assert_eq!(s, Side::Above);
        assert_eq!(p.bottom, 494);
        assert!(!overlaps(&p, &a));
    }

    #[test]
    fn below_when_near_top() {
        let a = r(100, 20, 400, 40);
        let (s, p) = place(a, WORK, 300, 100, 6, None, 60, CUR);
        assert_eq!(s, Side::Below);
        assert!(inside(&p, &WORK) && !overlaps(&p, &a));
    }

    #[test]
    fn clamps_to_right_edge() {
        let a = r(1800, 500, 1900, 520);
        let (_, p) = place(a, WORK, 400, 100, 6, None, 60, CUR);
        assert!(inside(&p, &WORK));
        assert_eq!(p.right, 1920);
    }

    #[test]
    fn tall_selection_goes_sideways() {
        let a = r(100, 30, 900, 1000);
        let (s, p) = place(a, WORK, 400, 300, 6, None, 60, CUR);
        assert_eq!(s, Side::Right);
        assert!(inside(&p, &WORK) && !overlaps(&p, &a));
    }

    #[test]
    fn sticky_side_kept_while_fits() {
        let a = r(100, 300, 400, 320);
        // 下方空间更大，但之前在上方且仍放得下
        let (s, _) = place(a, WORK, 300, 200, 6, Some(Side::Above), 60, CUR);
        assert_eq!(s, Side::Above);
    }

    #[test]
    fn sticky_flips_when_overflow() {
        let a = r(100, 300, 400, 320);
        let (s, p) = place(a, WORK, 300, 400, 6, Some(Side::Above), 60, CUR);
        assert_eq!(s, Side::Below);
        assert!(inside(&p, &WORK));
    }

    #[test]
    fn capped_and_scrollable_when_nothing_fits() {
        let a = r(100, 400, 400, 420);
        let (s, p) = place(a, WORK, 300, 2000, 6, None, 60, CUR);
        // 右侧可见面积最大
        assert_eq!(s, Side::Right);
        assert!(inside(&p, &WORK) && !overlaps(&p, &a));
    }

    #[test]
    fn full_screen_selection_overlays() {
        let a = r(0, 0, 1920, 1040);
        let (s, p) = place(a, WORK, 300, 200, 6, None, 60, CUR);
        assert_eq!(s, Side::Overlay);
        assert!(inside(&p, &WORK));
    }
}
