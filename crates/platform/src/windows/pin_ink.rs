//! 贴图涂鸦交互（pin 的子模块）：工具与草稿、类 Snipaste 的就地文字编辑、
//! 撤销历史、滚轮调节笔刷粗细与笔刷光标。
use super::*;
use std::rc::Rc;
use windows::Win32::UI::Input::Ime::{
    CFS_POINT, COMPOSITIONFORM, ImmGetContext, ImmReleaseContext, ImmSetCompositionWindow,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SetActiveWindow, SetFocus, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VK_BACK, VK_DELETE, VK_DOWN, VK_END, VK_HOME, VK_LEFT, VK_RIGHT, VK_SHIFT, VK_UP,
};

pub(super) const CARET_TIMER: usize = 7;
pub(super) const ESCAPE: u16 = bar::ESCAPE;

/// 每种工具的默认粗细（逻辑像素；文字为字号）与可调范围。
const SIZES: [(f32, f32, f32); 7] = [
    (3.0, 1.0, 40.0),    // 矩形 / 椭圆
    (3.0, 1.0, 40.0),    // 箭头 / 直线
    (3.0, 1.0, 40.0),    // 画笔
    (16.0, 4.0, 80.0),   // 记号笔
    (24.0, 6.0, 120.0),  // 马赛克
    (20.0, 10.0, 120.0), // 文字
    (20.0, 4.0, 120.0),  // 橡皮擦
];

fn slot(tool: Tool) -> usize {
    match tool {
        Tool::Rect | Tool::Ellipse => 0,
        Tool::Arrow | Tool::Line => 1,
        Tool::Pen => 2,
        Tool::Marker => 3,
        Tool::Mosaic => 4,
        Tool::Text => 5,
        Tool::Eraser => 6,
    }
}

struct TextEdit {
    text: Vec<char>,
    caret: usize,
    left: i32,
    top: i32,
    /// 开始编辑前的形状列表，用于取消与撤销。
    before: Vec<Rc<Shape>>,
    /// 再次编辑已有文字时：原位置和原形状。
    original: Option<(usize, Rc<Shape>)>,
    drag: Option<(P, (i32, i32))>,
    blink: bool,
    /// 最近一次排版的尺寸，用于命中测试。
    size: (i32, i32),
    pending_surrogate: Option<u16>,
}

/// 涂鸦状态；`Picture::pixels` 始终是原图叠加全部已提交形状的结果。
#[derive(Default)]
pub(super) struct Annotate {
    pub(super) base: Vec<u8>,
    shapes: Vec<Rc<Shape>>,
    history: Vec<Vec<Rc<Shape>>>,
    pub(super) tool: Option<Tool>,
    pub(super) color: u32,
    /// 正在绘制的点；折线最后一个点是跟随鼠标的预览点。
    draft: Option<Vec<P>>,
    edit: Option<TextEdit>,
    /// 鼠标在图片上的位置，用于绘制笔刷大小圆圈。
    hover: Option<P>,
    /// 最新一笔（形状下标, 工具）：滚轮调粗细时同步作用于它，类 Snipaste。
    last: Option<(usize, Tool)>,
    tracking: bool,
    pub(super) scale: f32,
    sizes: [f32; 7],
}
impl Annotate {
    pub(super) fn new(base: Vec<u8>, color: u32) -> Self {
        Self {
            base,
            color,
            scale: 1.0,
            sizes: SIZES.map(|s| s.0),
            ..Default::default()
        }
    }
    /// 已有标注（含进行中的草稿或文字）：截图选区随之锁定。
    pub(super) fn marked(&self) -> bool {
        !self.shapes.is_empty() || self.draft.is_some() || self.edit.is_some()
    }
    /// 关闭时转移原图和已提交的形状，释放撤销与当前编辑持有的引用。
    pub(super) fn into_closed(self) -> (Vec<u8>, Vec<Shape>) {
        let Self { base, shapes, history, edit, .. } = self;
        drop(history);
        drop(edit);
        (base, shapes.into_iter().map(Rc::unwrap_or_clone).collect())
    }
    /// 当前工具的物理像素粗细（文字为字号）。
    fn width(&self, tool: Tool) -> f32 {
        self.sizes[slot(tool)] * self.scale.max(1.0)
    }
    fn text_size(&self) -> i32 {
        self.width(Tool::Text).round() as i32
    }
    fn shape(&self, points: &[P]) -> Option<Shape> {
        let tool = self.tool?;
        let width = self.width(tool);
        let first = *points.first()?;
        let last = *points.last()?;
        let (points, closed, ink) = match tool {
            Tool::Arrow => {
                return Some(Shape::Arrow {
                    from: first,
                    to: last,
                    color: self.color,
                    width,
                });
            }
            Tool::Rect => (draw::rect(first, last), true, Ink::Solid),
            Tool::Ellipse => (draw::ellipse(first, last), true, Ink::Solid),
            Tool::Line => (vec![first, last], false, Ink::Solid),
            Tool::Pen => (points.to_vec(), false, Ink::Solid),
            Tool::Marker => (points.to_vec(), false, Ink::Marker),
            Tool::Mosaic => (
                points.to_vec(),
                false,
                Ink::Mosaic(mosaic_block(width)),
            ),
            Tool::Eraser => (points.to_vec(), false, Ink::Erase),
            Tool::Text => return None,
        };
        Some(Shape::Path {
            points,
            closed,
            ink,
            color: self.color,
            width,
        })
    }
}

fn mosaic_block(width: f32) -> i32 {
    (width * 0.42).round().clamp(4.0, 48.0) as i32
}

/// 以新粗细重建形状（马赛克块大小随之变化）；文字不受影响。
fn resized(shape: &Shape, new_width: f32) -> Option<Shape> {
    match shape {
        Shape::Arrow { from, to, color, .. } => Some(Shape::Arrow {
            from: *from,
            to: *to,
            color: *color,
            width: new_width,
        }),
        Shape::Path {
            points,
            closed,
            ink,
            color,
            ..
        } => Some(Shape::Path {
            points: points.clone(),
            closed: *closed,
            ink: match ink {
                Ink::Mosaic(_) => Ink::Mosaic(mosaic_block(new_width)),
                other => *other,
            },
            color: *color,
            width: new_width,
        }),
        Shape::Text { .. } => None,
    }
}

fn text_bounds(shape: &Shape) -> Option<(i32, i32, i32, i32)> {
    match shape {
        Shape::Text {
            left,
            top,
            width,
            height,
            ..
        } => Some((*left, *top, *width, *height)),
        _ => None,
    }
}

fn inside(at: P, (left, top, width, height): (i32, i32, i32, i32), pad: f32) -> bool {
    (left as f32 - pad..(left + width) as f32 + pad).contains(&at.0)
        && (top as f32 - pad..(top + height) as f32 + pad).contains(&at.1)
}

fn brush(tool: Tool) -> bool {
    matches!(tool, Tool::Pen | Tool::Marker | Tool::Mosaic | Tool::Eraser)
}

fn blend_px(canvas: &mut Canvas, x: i32, y: i32, bgr: [u8; 3], cov: f32) {
    if cov <= 0.0 || !(0..canvas.width).contains(&x) || !(0..canvas.height).contains(&y) {
        return;
    }
    let i = canvas.offset + y as usize * canvas.stride + x as usize * 4;
    for (k, v) in bgr.into_iter().enumerate() {
        let d = &mut canvas.data[i + k];
        *d = (f32::from(*d) + (f32::from(v) - f32::from(*d)) * cov.min(1.0)).round() as u8;
    }
}

/// 白色圆圈 + 内外深色描边，任何底色上都能看清笔刷范围。
fn ring(canvas: &mut Canvas, center: P, radius: f32) {
    let r = radius.max(1.5);
    let reach = (r + 3.0).ceil() as i32;
    let (cx, cy) = (center.0.floor() as i32, center.1.floor() as i32);
    for y in cy - reach..=cy + reach {
        for x in cx - reach..=cx + reach {
            let d = ((x as f32 + 0.5 - center.0).powi(2) + (y as f32 + 0.5 - center.1).powi(2)).sqrt();
            let dark = (1.0 - ((d - r).abs() - 0.6).max(0.0) / 1.0).clamp(0.0, 1.0)
                .max((1.0 - (d - r).abs() / 1.6).clamp(0.0, 1.0));
            blend_px(canvas, x, y, [40, 40, 40], dark * 0.55);
            let white = (0.5 + 0.5 - (d - r).abs()).clamp(0.0, 1.0);
            blend_px(canvas, x, y, [255, 255, 255], white);
        }
    }
}

fn put(canvas: &mut Canvas, x: i32, y: i32, bgr: [u8; 3]) {
    if (0..canvas.width).contains(&x) && (0..canvas.height).contains(&y) {
        let i = canvas.offset + y as usize * canvas.stride + x as usize * 4;
        canvas.data[i..i + 4].copy_from_slice(&[bgr[0], bgr[1], bgr[2], 255]);
    }
}

impl Picture {
    /// 把当前图片（含草稿与正在编辑的文字）写入已合成 DIB 的内容区，阴影保持不变。
    pub(super) fn paint_content(&mut self) {
        let Picture {
            pixels,
            width,
            height,
            view,
            folded,
            surface,
            ink,
            ..
        } = self;
        let stride = surface.width as usize * 4;
        let bytes = surface.bytes_mut();
        for y in 0..view.height {
            let src = ((view.y + y) as usize * *width as usize + view.x as usize) * 4;
            let dst = (y + SHADOW) as usize * stride + SHADOW as usize * 4;
            bytes[dst..dst + view.width as usize * 4]
                .copy_from_slice(&pixels[src..src + view.width as usize * 4]);
        }
        if *folded {
            return;
        }
        let mut canvas = Canvas {
            data: bytes,
            stride,
            offset: SHADOW as usize * stride + SHADOW as usize * 4,
            width: *width,
            height: *height,
        };
        if let Some(shape) = ink.draft.as_deref().and_then(|d| ink.shape(d)) {
            draw::draw(&shape, &mut canvas, &ink.base);
        }
        // 自由笔刷类工具：在鼠标处画出与粗细一致的圆圈。
        if let (Some(at), Some(tool)) = (ink.hover, ink.tool.filter(|t| brush(*t))) {
            ring(&mut canvas, at, ink.width(tool) / 2.0);
        }
        let size = ink.text_size();
        let color = ink.color;
        let Some(edit) = ink.edit.as_mut() else {
            return;
        };
        let text: String = edit.text.iter().collect();
        let layout = match pin_text::layout(&text, size) {
            Ok(layout) => layout,
            Err(e) => {
                log::warn!("文字排版失败: {e:#}");
                return;
            }
        };
        edit.size = (layout.width, layout.height);
        let shape = Shape::Text {
            left: edit.left,
            top: edit.top,
            width: layout.width,
            height: layout.height,
            mask: layout.mask,
            color,
            text: String::new(),
            size: 0.0,
        };
        draw::draw(&shape, &mut canvas, &ink.base);
        // 蓝白相间的虚线框，任何底色上都看得见。
        let pad = 4;
        let (x0, y0) = (edit.left - pad, edit.top - pad);
        let (x1, y1) = (edit.left + layout.width + pad, edit.top + layout.height + pad);
        let dash = |i: i32| if (i / 4) % 2 == 0 { [215, 120, 0] } else { [255, 255, 255] };
        for x in x0..=x1 {
            put(&mut canvas, x, y0, dash(x - x0));
            put(&mut canvas, x, y1, dash(x - x0));
        }
        for y in y0..=y1 {
            put(&mut canvas, x0, y, dash(y - y0));
            put(&mut canvas, x1, y, dash(y - y0));
        }
        if edit.blink {
            if let Ok((cx, cy)) = pin_text::caret(&edit.text, edit.caret, size) {
                let bgr = [(color >> 16) as u8, (color >> 8) as u8, color as u8];
                let thick = (size / 16).max(1);
                for y in 0..layout.line {
                    for t in 0..thick {
                        put(&mut canvas, edit.left + cx + t, edit.top + cy + y, bgr);
                    }
                }
            }
        }
    }
    /// 在刚建立的原图副本上重放历史记录里的标注。
    pub(super) fn restore_shapes(&mut self, shapes: Vec<Shape>) {
        self.ink.shapes = shapes.into_iter().map(Rc::new).collect();
        self.paint_shapes();
    }

    /// 从原图重放全部形状。
    fn rebuild(&mut self) {
        self.pixels.copy_from_slice(&self.ink.base);
        self.paint_shapes();
    }

    fn paint_shapes(&mut self) {
        let Picture {
            pixels,
            width,
            height,
            ink,
            ..
        } = self;
        let mut canvas = Canvas {
            data: pixels.as_mut_slice(),
            stride: *width as usize * 4,
            offset: 0,
            width: *width,
            height: *height,
        };
        for shape in &ink.shapes {
            draw::draw(shape, &mut canvas, &ink.base);
        }
    }
    pub(super) fn commit(&mut self, shape: Shape) {
        let Picture {
            pixels,
            width,
            height,
            ink,
            ..
        } = self;
        let mut canvas = Canvas {
            data: pixels.as_mut_slice(),
            stride: *width as usize * 4,
            offset: 0,
            width: *width,
            height: *height,
        };
        draw::draw(&shape, &mut canvas, &ink.base);
        ink.history.push(ink.shapes.clone());
        ink.shapes.push(Rc::new(shape));
    }
    fn undo(&mut self) -> bool {
        self.ink.last = None;
        let Some(previous) = self.ink.history.pop() else {
            return false;
        };
        self.ink.shapes = previous;
        self.rebuild();
        true
    }
}

/// 截图完成后立即把贴图设为前台（选中）。后台进程直接 SetForegroundWindow
/// 可能被系统拒绝，失败时临时挂接前台线程的输入队列再试。
pub(super) fn activate(hwnd: HWND) {
    use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
    unsafe {
        if SetForegroundWindow(hwnd).as_bool() && GetForegroundWindow() == hwnd {
            return;
        }
        let foreground = GetForegroundWindow();
        let other = GetWindowThreadProcessId(foreground, None);
        let me = GetCurrentThreadId();
        let attached = other != 0 && other != me && AttachThreadInput(me, other, true).as_bool();
        let _ = BringWindowToTop(hwnd);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetActiveWindow(hwnd);
        let _ = SetFocus(Some(hwnd));
        if attached {
            let _ = AttachThreadInput(me, other, false);
        }
    }
}

pub(super) fn with_picture<R>(f: impl FnOnce(&mut Picture) -> R) -> R {
    PICTURE.with(|p| f(p.borrow_mut().as_mut().expect("pin picture")))
}

pub(super) fn redraw(hwnd: HWND) -> Result<()> {
    with_picture(|p| {
        p.paint_view();
        p.refresh_zoom()
    })?;
    render(hwnd, None)
}

fn key_down(key: VIRTUAL_KEY) -> bool {
    unsafe { GetKeyState(i32::from(key.0)) < 0 }
}

pub(super) fn editing() -> bool {
    PICTURE.with(|p| p.borrow().as_ref().is_some_and(|p| p.ink.edit.is_some()))
}

/// 本线程只有贴图和工具栏，键盘消息都属于当前贴图。
pub(super) fn shortcut(msg: &MSG) -> Option<u16> {
    if msg.message != WM_KEYDOWN || key_down(VK_MENU) {
        return None;
    }
    let key = msg.wParam.0;
    if key > 0xff {
        return None;
    }
    if key_down(VK_CONTROL) {
        return match key as u8 {
            b'C' => Some(bar::COPY),
            b'S' => Some(bar::SAVE),
            b'T' => Some(bar::PIN),
            b'Z' => Some(bar::UNDO),
            b'1' => Some(bar::RECT_TOOL),
            b'2' => Some(bar::LINE_TOOL),
            b'3' => Some(bar::PEN),
            b'4' => Some(bar::MARKER),
            b'5' => Some(bar::MOSAIC),
            b'6' => Some(bar::TEXT),
            _ => None,
        };
    }
    if key == usize::from(b'E') {
        Some(bar::ERASER)
    } else if key == usize::from(VK_ESCAPE.0) {
        Some(ESCAPE)
    } else if key == usize::from(VK_SPACE.0) {
        Some(TOGGLE_TOOLBAR)
    } else {
        None
    }
}

/// 文字编辑时优先处理按键与字符；返回 true 表示消息已消费。
/// Ctrl 组合键（复制、保存、切换工具）交回快捷键处理，它们会先提交文字。
pub(super) fn pump(hwnd: HWND, msg: &MSG) -> bool {
    if !editing() {
        return false;
    }
    match msg.message {
        WM_CHAR => {
            report_render_error(char_input(hwnd, msg.wParam.0 as u16));
            true
        }
        WM_KEYDOWN => {
            let key = msg.wParam.0 as u16;
            let ctrl = key_down(VK_CONTROL);
            let result = match VIRTUAL_KEY(key) {
                VK_ESCAPE => finish_text(hwnd),
                // 与工具栏提示一致：Enter 确定，Shift+Enter 换行。
                VK_RETURN if key_down(VK_SHIFT) => edit_text(hwnd, |e| insert(e, &['\n'])),
                VK_RETURN => finish_text(hwnd),
                VK_BACK => edit_text(hwnd, |e| {
                    if e.caret > 0 {
                        e.caret -= 1;
                        e.text.remove(e.caret);
                    }
                }),
                VK_DELETE => edit_text(hwnd, |e| {
                    if e.caret < e.text.len() {
                        e.text.remove(e.caret);
                    }
                }),
                VK_LEFT => edit_text(hwnd, |e| e.caret = e.caret.saturating_sub(1)),
                VK_RIGHT => edit_text(hwnd, |e| e.caret = (e.caret + 1).min(e.text.len())),
                VK_HOME => edit_text(hwnd, |e| e.caret = line_start(&e.text, e.caret)),
                VK_END => edit_text(hwnd, |e| e.caret = line_end(&e.text, e.caret)),
                VK_UP => edit_text(hwnd, |e| e.caret = vertical(&e.text, e.caret, false)),
                VK_DOWN => edit_text(hwnd, |e| e.caret = vertical(&e.text, e.caret, true)),
                _ if ctrl && key == u16::from(b'V') => paste(hwnd),
                _ if ctrl && key == u16::from(b'Z') => cancel_text(hwnd),
                _ if ctrl => return false,
                _ => {
                    // 交给 TranslateMessage 生成 WM_CHAR（含输入法）。
                    unsafe {
                        let _ = TranslateMessage(msg);
                        DispatchMessageW(msg);
                    }
                    return true;
                }
            };
            report_render_error(result);
            true
        }
        _ => false,
    }
}

fn line_start(text: &[char], caret: usize) -> usize {
    text[..caret].iter().rposition(|&c| c == '\n').map_or(0, |i| i + 1)
}
fn line_end(text: &[char], caret: usize) -> usize {
    text[caret..]
        .iter()
        .position(|&c| c == '\n')
        .map_or(text.len(), |i| caret + i)
}
/// 上下移动光标，按字符列近似对齐。
fn vertical(text: &[char], caret: usize, down: bool) -> usize {
    let start = line_start(text, caret);
    let column = caret - start;
    if down {
        let end = line_end(text, caret);
        if end >= text.len() {
            return caret;
        }
        let next = end + 1;
        (next + column).min(line_end(text, next))
    } else {
        if start == 0 {
            return caret;
        }
        let previous = line_start(text, start - 1);
        (previous + column).min(start - 1)
    }
}
fn insert(edit: &mut TextEdit, chars: &[char]) {
    for &c in chars {
        edit.text.insert(edit.caret, c);
        edit.caret += 1;
    }
}

pub(super) fn char_input(hwnd: HWND, unit: u16) -> Result<()> {
    edit_text(hwnd, |e| {
        if (0xd800..0xdc00).contains(&unit) {
            e.pending_surrogate = Some(unit);
            return;
        }
        let units: Vec<u16> = e.pending_surrogate.take().into_iter().chain([unit]).collect();
        let chars: Vec<char> = char::decode_utf16(units)
            .filter_map(|c| c.ok())
            .map(|c| if c == '\t' { ' ' } else { c })
            .filter(|c| !c.is_control())
            .collect();
        insert(e, &chars);
    })
}

fn paste(hwnd: HWND) -> Result<()> {
    let text = super::super::clipboard::get_text().unwrap_or_default();
    let chars: Vec<char> = text
        .replace("\r\n", "\n")
        .chars()
        .map(|c| if c == '\t' { ' ' } else { c })
        .filter(|c| *c == '\n' || !c.is_control())
        .collect();
    edit_text(hwnd, |e| insert(e, &chars))
}

/// 修改文字后重置光标闪烁、更新输入法候选窗位置并重绘。
fn edit_text(hwnd: HWND, f: impl FnOnce(&mut TextEdit)) -> Result<()> {
    with_picture(|p| {
        if let Some(e) = p.ink.edit.as_mut() {
            f(e);
            e.blink = true;
        }
    });
    unsafe {
        SetTimer(Some(hwnd), CARET_TIMER, GetCaretBlinkTime().max(300), None);
    }
    place_ime(hwnd);
    redraw(hwnd)
}

fn place_ime(hwnd: HWND) {
    let position = with_picture(|p| {
        let size = p.ink.text_size();
        let e = p.ink.edit.as_ref()?;
        let (cx, cy) = pin_text::caret(&e.text, e.caret, size).ok()?;
        let (width, height) = p.display_size();
        Some(POINT {
            x: SHADOW
                + ((e.left + cx - p.view.x) as f32 * width as f32 / p.view.width as f32).round()
                    as i32,
            y: SHADOW
                + ((e.top + cy - p.view.y) as f32 * height as f32 / p.view.height as f32).round()
                    as i32,
        })
    });
    let Some(position) = position else { return };
    unsafe {
        let context = ImmGetContext(hwnd);
        if !context.is_invalid() {
            let form = COMPOSITIONFORM {
                dwStyle: CFS_POINT,
                ptCurrentPos: position,
                ..Default::default()
            };
            let _ = ImmSetCompositionWindow(context, &form);
            let _ = ImmReleaseContext(hwnd, context);
        }
    }
}

pub(super) fn caret_tick(hwnd: HWND) -> Result<()> {
    let editing = with_picture(|p| {
        p.ink.edit.as_mut().map(|e| {
            e.blink = !e.blink;
        })
    });
    if editing.is_some() {
        redraw(hwnd)
    } else {
        unsafe {
            let _ = KillTimer(Some(hwnd), CARET_TIMER);
        }
        Ok(())
    }
}

fn begin_text(hwnd: HWND, edit: TextEdit) -> Result<()> {
    with_picture(|p| p.ink.edit = Some(edit));
    unsafe {
        let _ = SetFocus(Some(hwnd));
    }
    sync_bar();
    edit_text(hwnd, |_| {})
}

fn begin_new_text(hwnd: HWND, at: P) -> Result<()> {
    let edit = with_picture(|p| {
        let line = (p.ink.text_size() as f32 * 1.33) as i32;
        TextEdit {
            text: Vec::new(),
            caret: 0,
            left: at.0 as i32,
            top: at.1 as i32 - line / 2,
            before: p.ink.shapes.clone(),
            original: None,
            drag: None,
            blink: true,
            size: (0, 0),
            pending_surrogate: None,
        }
    });
    begin_text(hwnd, edit)
}

/// 再次编辑已有文字：从图片上取下，恢复其颜色与字号。
fn begin_existing_text(hwnd: HWND, index: usize, at: P) -> Result<()> {
    let edit = with_picture(|p| {
        let before = p.ink.shapes.clone();
        let shape = p.ink.shapes.remove(index);
        let Shape::Text {
            left,
            top,
            color,
            text,
            size,
            ..
        } = &*shape
        else {
            unreachable!("text hit test");
        };
        p.ink.color = *color;
        p.ink.sizes[slot(Tool::Text)] = *size;
        let text: Vec<char> = text.chars().collect();
        let edit = TextEdit {
            caret: text.len(),
            text,
            left: *left,
            top: *top,
            before,
            original: Some((index, shape.clone())),
            drag: Some((at, (*left, *top))),
            blink: true,
            size: (0, 0),
            pending_surrogate: None,
        };
        p.rebuild();
        edit
    });
    begin_text(hwnd, edit)
}

/// 提交文字；内容为空等同删除，未作任何修改则不产生撤销记录。
pub(super) fn finish_text(hwnd: HWND) -> Result<()> {
    if editing() {
        with_picture(|p| p.ink.last = None);
    }
    unsafe {
        let _ = KillTimer(Some(hwnd), CARET_TIMER);
    }
    let Some(edit) = with_picture(|p| p.ink.edit.take()) else {
        return Ok(());
    };
    let (size, scale, color) = with_picture(|p| {
        (p.ink.text_size(), p.ink.sizes[slot(Tool::Text)], p.ink.color)
    });
    let text: String = edit.text.iter().collect();
    let text = text.trim_end().to_string();
    let shape = if text.trim().is_empty() {
        None
    } else {
        let layout = pin_text::layout(&text, size)?;
        Some(Rc::new(Shape::Text {
            left: edit.left,
            top: edit.top,
            width: layout.width,
            height: layout.height,
            mask: layout.mask,
            color,
            text: text.clone(),
            size: scale,
        }))
    };
    let unchanged = match (&edit.original, &shape) {
        (None, None) => true,
        (Some((_, original)), Some(_)) => matches!(&**original, Shape::Text { left, top, color: c, text: t, size: s, .. }
            if *left == edit.left && *top == edit.top && *c == color && *t == text && *s == scale),
        _ => false,
    };
    with_picture(|p| {
        if unchanged {
            p.ink.shapes = edit.before;
        } else {
            if let Some(shape) = shape {
                let index = edit.original.as_ref().map_or(p.ink.shapes.len(), |(i, _)| *i);
                p.ink.shapes.insert(index.min(p.ink.shapes.len()), shape);
            }
            p.ink.history.push(edit.before);
        }
        p.rebuild();
    });
    redraw(hwnd)
}

/// 编辑中 Ctrl+Z：放弃本次编辑，恢复到编辑前。
fn cancel_text(hwnd: HWND) -> Result<()> {
    with_picture(|p| p.ink.last = None);
    unsafe {
        let _ = KillTimer(Some(hwnd), CARET_TIMER);
    }
    with_picture(|p| {
        if let Some(edit) = p.ink.edit.take() {
            p.ink.shapes = edit.before;
            p.rebuild();
        }
    });
    redraw(hwnd)
}

fn tool_button(tool: Option<Tool>) -> u16 {
    match tool {
        None => 0,
        Some(Tool::Rect | Tool::Ellipse) => bar::RECT_TOOL,
        Some(Tool::Arrow | Tool::Line) => bar::LINE_TOOL,
        Some(Tool::Pen) => bar::PEN,
        Some(Tool::Marker) => bar::MARKER,
        Some(Tool::Mosaic) => bar::MOSAIC,
        Some(Tool::Text) => bar::TEXT,
        Some(Tool::Eraser) => bar::ERASER,
    }
}

fn sync_bar() {
    let (tool, color) = with_picture(|p| (p.ink.tool, p.ink.color));
    bar::sync(
        tool_button(tool),
        matches!(tool, Some(Tool::Ellipse | Tool::Line)),
        COLORREF(color),
    );
}

/// 提交进行中的草稿；过小的矩形 / 线段视为误触丢弃。
fn finish_draft(hwnd: HWND) -> Result<()> {
    let changed = with_picture(|p| {
        let Some(points) = p.ink.draft.take() else {
            return false;
        };
        match p.ink.tool {
            Some(Tool::Arrow | Tool::Line) => {
                let (a, b) = (points[0], *points.last().unwrap());
                if (a.0 - b.0).hypot(a.1 - b.1) < 3.0 {
                    return true;
                }
            }
            Some(Tool::Rect | Tool::Ellipse) => {
                let (a, b) = (points[0], *points.last().unwrap());
                if (a.0 - b.0).abs() < 2.0 || (a.1 - b.1).abs() < 2.0 {
                    return true;
                }
            }
            _ => {}
        }
        if let Some(shape) = p.ink.shape(&points) {
            p.commit(shape);
            p.ink.last = p.ink.tool.map(|tool| (p.ink.shapes.len() - 1, tool));
        }
        true
    });
    if changed { redraw(hwnd) } else { Ok(()) }
}

/// 切换工具、复制、保存前先落定草稿和文字。
pub(super) fn settle(hwnd: HWND) -> Result<()> {
    finish_text(hwnd)?;
    finish_draft(hwnd)
}

fn select_tool(hwnd: HWND, id: u16) -> Result<()> {
    settle(hwnd)?;
    with_picture(|p| p.ink.last = None);
    if with_picture(|p| p.folded) {
        toggle_fold(hwnd)?;
    }
    with_picture(|p| {
        let current = p.ink.tool;
        p.ink.tool = match id {
            // Ctrl+1 在矩形和椭圆之间切换。
            bar::RECT_TOOL => Some(if current == Some(Tool::Rect) {
                Tool::Ellipse
            } else {
                Tool::Rect
            }),
            // Ctrl+2 在箭头（默认）和直线之间切换。
            bar::LINE_TOOL => Some(if current == Some(Tool::Arrow) {
                Tool::Line
            } else {
                Tool::Arrow
            }),
            _ => {
                let tool = match id {
                    bar::PEN => Tool::Pen,
                    bar::MARKER => Tool::Marker,
                    bar::MOSAIC => Tool::Mosaic,
                    bar::TEXT => Tool::Text,
                    _ => Tool::Eraser,
                };
                (current != Some(tool)).then_some(tool)
            }
        };
    });
    sync_bar();
    refresh_cursor(hwnd);
    Ok(())
}

fn set_color(hwnd: HWND, color: u32) -> Result<()> {
    with_picture(|p| p.ink.color = color);
    sync_bar();
    if editing() {
        // 颜色实时作用于正在编辑的文字，焦点还给贴图继续输入。
        unsafe {
            let _ = SetFocus(Some(hwnd));
        }
        return redraw(hwnd);
    }
    Ok(())
}

/// 完整展开的系统调色板；自定义颜色在本进程内保留。
fn pick_color(owner: HWND, initial: u32) -> Option<u32> {
    use std::sync::Mutex;
    use windows::Win32::UI::Controls::Dialogs::*;
    static CUSTOM: Mutex<[u32; 16]> = Mutex::new([0xffffff; 16]);
    let mut custom = CUSTOM
        .lock()
        .map(|c| *c)
        .unwrap_or([0xffffff; 16])
        .map(COLORREF);
    let mut dialog = CHOOSECOLORW {
        lStructSize: size_of::<CHOOSECOLORW>() as u32,
        hwndOwner: owner,
        rgbResult: COLORREF(initial),
        lpCustColors: custom.as_mut_ptr(),
        Flags: CC_FULLOPEN | CC_RGBINIT | CC_ANYCOLOR,
        ..Default::default()
    };
    let chosen = unsafe { ChooseColorW(&mut dialog) }.as_bool();
    if let Ok(mut c) = CUSTOM.lock() {
        *c = custom.map(|c| c.0);
    }
    chosen.then_some(dialog.rgbResult.0)
}

fn image_point(p: &Picture, client: POINT) -> P {
    let (width, height) = p.display_size();
    (
        p.view.x as f32 + ((client.x - SHADOW) as f32 + 0.5) * p.view.width as f32 / width as f32,
        p.view.y as f32 + ((client.y - SHADOW) as f32 + 0.5) * p.view.height as f32 / height as f32,
    )
}

fn hit_text(p: &Picture, at: P) -> Option<usize> {
    p.ink
        .shapes
        .iter()
        .rposition(|s| text_bounds(s).is_some_and(|b| inside(at, b, 3.0)))
}

fn over_editing(p: &Picture, at: P) -> bool {
    p.ink
        .edit
        .as_ref()
        .is_some_and(|e| inside(at, (e.left, e.top, e.size.0, e.size.1), 6.0))
}

pub(super) fn pointer_down(hwnd: HWND, client: POINT) -> Result<()> {
    let (tool, at) = with_picture(|p| (p.ink.tool, image_point(p, client)));
    let Some(tool) = tool else { return Ok(()) };
    match tool {
        Tool::Text => {
            // 类 Snipaste：拖动文字框移动；点框外提交；点已有文字重新编辑并可拖动。
            if editing() {
                let grabbed = with_picture(|p| {
                    let over = over_editing(p, at);
                    if let (true, Some(e)) = (over, p.ink.edit.as_mut()) {
                        e.drag = Some((at, (e.left, e.top)));
                    }
                    over
                });
                if grabbed {
                    unsafe {
                        SetCapture(hwnd);
                    }
                    return Ok(());
                }
                return finish_text(hwnd);
            }
            if let Some(index) = with_picture(|p| hit_text(p, at)) {
                unsafe {
                    SetCapture(hwnd);
                }
                return begin_existing_text(hwnd, index, at);
            }
            begin_new_text(hwnd, at)
        }
        _ => {
            unsafe {
                SetCapture(hwnd);
            }
            with_picture(|p| {
                p.ink.draft = Some(if !brush(tool) {
                    vec![at, at]
                } else {
                    vec![at]
                })
            });
            redraw(hwnd)
        }
    }
}

pub(super) fn pointer_move(hwnd: HWND, client: POINT, shift: bool) -> Result<()> {
    let track = with_picture(|p| {
        let tracked = p.ink.tracking;
        p.ink.tracking = true;
        !tracked
    });
    if track {
        // 鼠标离开贴图时收到 WM_MOUSELEAVE，隐藏笔刷圆圈。
        unsafe {
            let _ = TrackMouseEvent(&mut TRACKMOUSEEVENT {
                cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: hwnd,
                dwHoverTime: 0,
            });
        }
    }
    let changed = with_picture(|p| {
        let at = image_point(p, client);
        let hover_changed = p.ink.tool.is_some_and(brush) && p.ink.hover != Some(at);
        if hover_changed {
            p.ink.hover = Some(at);
        }
        if let Some(e) = p.ink.edit.as_mut() {
            let Some((from, origin)) = e.drag else {
                return false;
            };
            e.left = origin.0 + (at.0 - from.0).round() as i32;
            e.top = origin.1 + (at.1 - from.1).round() as i32;
            return true;
        }
        let tool = p.ink.tool;
        let Some(points) = p.ink.draft.as_mut() else {
            return hover_changed;
        };
        match tool {
            Some(Tool::Rect | Tool::Ellipse) => {
                let a = points[0];
                let mut b = at;
                if shift {
                    // Shift 约束为正方形 / 圆形。
                    let side = (b.0 - a.0).abs().max((b.1 - a.1).abs());
                    b = (a.0 + side.copysign(b.0 - a.0), a.1 + side.copysign(b.1 - a.1));
                }
                points[1] = b;
            }
            Some(Tool::Arrow | Tool::Line) => {
                let a = points[0];
                let mut b = at;
                if shift {
                    // Shift 锁定到 45° 的整数倍。
                    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
                    let step = std::f32::consts::FRAC_PI_4;
                    let angle = (dy.atan2(dx) / step).round() * step;
                    let len = dx.hypot(dy);
                    b = (a.0 + len * angle.cos(), a.1 + len * angle.sin());
                }
                points[1] = b;
            }
            _ => {
                let last = *points.last().unwrap();
                if (last.0 - at.0).abs() < 1.0 && (last.1 - at.1).abs() < 1.0 {
                    return false;
                }
                points.push(at);
            }
        }
        true
    });
    if changed {
        if editing() {
            place_ime(hwnd);
        }
        redraw(hwnd)
    } else {
        Ok(())
    }
}

pub(super) fn pointer_up(hwnd: HWND) -> Result<()> {
    unsafe {
        if GetCapture() == hwnd {
            let _ = ReleaseCapture();
        }
    }
    with_picture(|p| {
        if let Some(e) = p.ink.edit.as_mut() {
            e.drag = None;
        }
    });
    if with_picture(|p| p.ink.tool != Some(Tool::Text)) {
        finish_draft(hwnd)?;
    }
    Ok(())
}

pub(super) fn mouse_leave(hwnd: HWND) -> Result<()> {
    let changed = with_picture(|p| {
        p.ink.tracking = false;
        p.ink.hover.take().is_some()
    });
    if changed { redraw(hwnd) } else { Ok(()) }
}

/// 右键：结束拖动中的图形 / 提交文字 / 退出涂鸦。
pub(super) fn right_click(hwnd: HWND) -> Result<()> {
    if with_picture(|p| p.ink.draft.is_some()) {
        finish_draft(hwnd)
    } else if editing() {
        finish_text(hwnd)
    } else {
        with_picture(|p| p.ink.tool = None);
        sync_bar();
        redraw(hwnd)
    }
}

/// Esc 的第一步：提交正在编辑的文字或取消草稿；返回 false 表示没有可处理的，
/// 由调用方决定退出截图 / 完成编辑 / 关闭贴图。
pub(super) fn escape_edit(hwnd: HWND) -> Result<bool> {
    if editing() {
        finish_text(hwnd)?;
        return Ok(true);
    }
    if with_picture(|p| p.ink.draft.take().is_some()) {
        redraw(hwnd)?;
        return Ok(true);
    }
    Ok(false)
}

/// 完成编辑：落定草稿和文字并取消当前工具。
pub(super) fn drop_tool(hwnd: HWND) -> Result<()> {
    settle(hwnd)?;
    if with_picture(|p| p.ink.tool.take().is_some()) {
        sync_bar();
        refresh_cursor(hwnd);
        redraw(hwnd)?;
    }
    Ok(())
}

/// 涂鸦相关命令；返回 false 表示不是涂鸦命令。
pub(super) fn command(hwnd: HWND, id: u16) -> Result<bool> {
    match id {
        bar::RECT_TOOL
        | bar::LINE_TOOL
        | bar::PEN
        | bar::MARKER
        | bar::MOSAIC
        | bar::TEXT
        | bar::ERASER => select_tool(hwnd, id)?,
        bar::UNDO => {
            if editing() {
                cancel_text(hwnd)?;
            } else if with_picture(|p| p.ink.draft.take().is_some() || p.undo()) {
                redraw(hwnd)?;
            }
        }
        bar::PALETTE => {
            let initial = with_picture(|p| {
                p.exporting = true;
                p.ink.color
            });
            bar::busy(true);
            let chosen = pick_color(hwnd, initial);
            with_picture(|p| p.exporting = false);
            bar::busy(false);
            if let Some(color) = chosen {
                set_color(hwnd, color)?;
            }
        }
        id if (bar::SWATCH..bar::SWATCH + bar::SWATCHES.len() as u16).contains(&id) => {
            set_color(hwnd, bar::SWATCHES[(id - bar::SWATCH) as usize])?;
        }
        _ => return Ok(false),
    }
    Ok(true)
}

/// 涂鸦模式（已选中且有工具）下滚轮调节当前工具粗细 / 字号；返回是否已处理。
pub(super) fn wheel(hwnd: HWND, delta: i32) -> Result<bool> {
    let changed = with_picture(|p| {
        let tool = p.ink.tool.filter(|_| p.selected && !p.folded)?;
        p.wheel_remainder += delta;
        let steps = p.wheel_remainder / 120;
        p.wheel_remainder %= 120;
        let i = slot(tool);
        let (_, min, max) = SIZES[i];
        let mut size = p.ink.sizes[i];
        for _ in 0..steps.abs() {
            let step = (size * 0.1).round().max(1.0);
            size += step.copysign(steps as f32);
        }
        let size = size.clamp(min, max);
        if size == p.ink.sizes[i] {
            return Some(());
        }
        p.ink.sizes[i] = size;
        // 最新一笔若出自当前工具，随滚轮一起变粗 / 变细。
        if let Some((index, made_by)) = p.ink.last {
            let width = p.ink.width(tool);
            let same = made_by == tool && index < p.ink.shapes.len();
            if let Some(shape) = same.then(|| resized(&p.ink.shapes[index], width)).flatten() {
                p.ink.shapes[index] = Rc::new(shape);
                p.rebuild();
            } else if !same {
                p.ink.last = None;
            }
        }
        Some(())
    });
    if changed.is_none() {
        return Ok(false);
    }
    refresh_cursor(hwnd);
    if editing() {
        place_ime(hwnd);
    }
    redraw(hwnd)?;
    Ok(true)
}

fn refresh_cursor(hwnd: HWND) {
    let mut cursor = POINT::default();
    unsafe {
        if GetCursorPos(&mut cursor).is_ok() && WindowFromPoint(cursor) == hwnd {
            let _ = ScreenToClient(hwnd, &mut cursor);
            if let Some(handle) = cursor_for(hwnd, cursor) {
                SetCursor(Some(handle));
            }
        }
    }
}

/// 客户区光标：文字工具在可拖动处显示移动光标，其余工具显示与粗细一致的圆形笔刷。
pub(super) fn cursor_for(_hwnd: HWND, client: POINT) -> Option<HCURSOR> {
    let (tool, width, over_text) = with_picture(|p| {
        let at = image_point(p, client);
        let tool = p.ink.tool?;
        let over = tool == Tool::Text && (over_editing(p, at) || (p.ink.edit.is_none() && hit_text(p, at).is_some()));
        Some((tool, p.ink.width(tool), over))
    })?;
    unsafe {
        if tool == Tool::Text {
            return LoadCursorW(None, if over_text { IDC_SIZEALL } else { IDC_IBEAM }).ok();
        }
    }
    let _ = width;
    brush_cursor(1.0)
}

thread_local! { static BRUSH: RefCell<Option<(i32, HCURSOR)>> = const { RefCell::new(None) }; }

/// 生成直径为 `width` 的圆形笔刷光标（白圈 + 深色描边 + 中心十字），按直径缓存。
fn brush_cursor(width: f32) -> Option<HCURSOR> {
    let d = width.round().clamp(1.0, 240.0) as i32;
    if let Some(cached) = BRUSH.with(|b| b.borrow().filter(|(k, _)| *k == d).map(|(_, c)| c)) {
        return Some(cached);
    }
    let size = (d + 8).max(25) | 1;
    let c = size as f32 / 2.0;
    let r = d as f32 / 2.0;
    let mut argb = vec![0u8; (size * size * 4) as usize];
    for y in 0..size {
        for x in 0..size {
            let (px, py) = (x as f32 + 0.5 - c, y as f32 + 0.5 - c);
            let dist = (px * px + py * py).sqrt();
            let ring = |radius: f32, w: f32| (w / 2.0 + 0.5 - (dist - radius).abs()).clamp(0.0, 1.0);
            let arm = |a: f32, b: f32| {
                if a.abs() <= 5.0 && b.abs() < 0.5 { 1.0f32 } else { 0.0 }
            };
            let cross = arm(px, py).max(arm(py, px));
            let cross_dark = {
                let near = |a: f32, b: f32| a.abs() <= 6.0 && b.abs() < 1.5;
                if near(px, py) || near(py, px) { 1.0f32 } else { 0.0 }
            };
            let (white, dark) = if d >= 6 {
                (ring(r, 1.2).max(cross), ring(r + 1.1, 1.0).max(ring(r - 1.1, 1.0)).max(cross_dark))
            } else {
                (cross, cross_dark)
            };
            let alpha = white + dark * (1.0 - white) * 0.8;
            if alpha <= 0.0 {
                continue;
            }
            let value = (255.0 * white / alpha).round() as u8;
            let i = ((y * size + x) * 4) as usize;
            argb[i..i + 4].copy_from_slice(&[value, value, value, (alpha * 255.0).round() as u8]);
        }
    }
    let cursor = unsafe {
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: size,
                biHeight: -size,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let color = CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0).ok()?;
        std::ptr::copy_nonoverlapping(argb.as_ptr(), bits.cast::<u8>(), argb.len());
        let zeros = vec![0u8; (((size + 15) / 16) * 2 * size) as usize];
        let mask = CreateBitmap(size, size, 1, 1, Some(zeros.as_ptr().cast()));
        let icon = CreateIconIndirect(&ICONINFO {
            fIcon: false.into(),
            xHotspot: (size / 2) as u32,
            yHotspot: (size / 2) as u32,
            hbmMask: mask,
            hbmColor: color,
        });
        let _ = DeleteObject(HGDIOBJ(color.0));
        let _ = DeleteObject(HGDIOBJ(mask.0));
        HCURSOR(icon.ok()?.0)
    };
    let old = BRUSH.with(|b| b.borrow_mut().replace((d, cursor)));
    if let Some((_, old)) = old {
        unsafe {
            SetCursor(Some(cursor));
            let _ = DestroyCursor(old);
        }
    }
    Some(cursor)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn caret_navigation_respects_lines() {
        let text: Vec<char> = "abc\nde\nfghij".chars().collect();
        assert_eq!(line_start(&text, 5), 4);
        assert_eq!(line_end(&text, 5), 6);
        assert_eq!(vertical(&text, 2, true), 6); // 第二行较短，落在行尾
        assert_eq!(vertical(&text, 5, true), 8);
        assert_eq!(vertical(&text, 9, false), 6);
        assert_eq!(vertical(&text, 1, false), 1);
    }
    #[test]
    fn resizing_keeps_geometry_and_rescales_mosaic_blocks() {
        let arrow = Shape::Arrow {
            from: (1.0, 2.0),
            to: (30.0, 40.0),
            color: 7,
            width: 3.0,
        };
        assert!(matches!(
            resized(&arrow, 9.0),
            Some(Shape::Arrow { from: (1.0, 2.0), to: (30.0, 40.0), color: 7, width }) if width == 9.0
        ));
        let mosaic = Shape::Path {
            points: vec![(0.0, 0.0), (5.0, 5.0)],
            closed: false,
            ink: Ink::Mosaic(mosaic_block(24.0)),
            color: 0,
            width: 24.0,
        };
        let Some(Shape::Path { points, ink, width, .. }) = resized(&mosaic, 60.0) else {
            panic!("path stays a path");
        };
        assert_eq!(points, vec![(0.0, 0.0), (5.0, 5.0)]);
        assert_eq!((ink, width), (Ink::Mosaic(mosaic_block(60.0)), 60.0));
        assert!(mosaic_block(60.0) > mosaic_block(24.0));
    }

    #[test]
    fn brush_sizes_have_sane_defaults_and_ranges() {
        for (default, min, max) in SIZES {
            assert!(min <= default && default <= max);
        }
        let ink = Annotate::new(Vec::new(), 0);
        assert_eq!(ink.width(Tool::Pen), 3.0);
        assert_eq!(ink.text_size(), 20);
    }
}
