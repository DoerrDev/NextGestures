//! 贴图文字排版：按行测量、栅格化为抗锯齿灰度遮罩、计算光标位置。
//! 文字直接画在贴图上（无输入框背景），编辑由 pin_ink 自绘光标和虚线框。
use anyhow::{Result, ensure};
use windows::Win32::Foundation::{COLORREF, RECT, SIZE};
use windows::Win32::Graphics::Gdi::*;

pub(super) struct Layout {
    pub width: i32,
    pub height: i32,
    pub line: i32,
    pub mask: Vec<u8>,
}

fn font(size: i32) -> HFONT {
    let mut font = LOGFONTW {
        lfHeight: -size,
        lfWeight: 500,
        lfQuality: ANTIALIASED_QUALITY,
        ..Default::default()
    };
    let face: Vec<u16> = "Microsoft YaHei UI".encode_utf16().collect();
    font.lfFaceName[..face.len()].copy_from_slice(&face);
    unsafe { CreateFontIndirectW(&font) }
}

/// 在内存 DC 上选入字体后执行 `f(dc, 行高)`。
fn with_font<R>(size: i32, f: impl FnOnce(HDC, HFONT, i32) -> R) -> Result<R> {
    unsafe {
        let font = font(size.max(1));
        ensure!(!font.is_invalid(), "创建字体失败");
        let dc = CreateCompatibleDC(None);
        let previous = SelectObject(dc, HGDIOBJ(font.0));
        let mut metrics = TEXTMETRICW::default();
        let _ = GetTextMetricsW(dc, &mut metrics);
        let result = f(dc, font, metrics.tmHeight.max(1));
        SelectObject(dc, previous);
        let _ = DeleteDC(dc);
        let _ = DeleteObject(HGDIOBJ(font.0));
        Ok(result)
    }
}

fn extent(dc: HDC, line: &str) -> i32 {
    let wide: Vec<u16> = line.encode_utf16().collect();
    let mut size = SIZE::default();
    if !wide.is_empty() {
        unsafe {
            let _ = GetTextExtentPoint32W(dc, &wide, &mut size);
        }
    }
    size.cx
}

/// 排版并栅格化（白字黑底取绿色通道为覆盖率）；空文本得到一行高的空遮罩。
pub(super) fn layout(text: &str, size: i32) -> Result<Layout> {
    let lines: Vec<&str> = text.split('\n').collect();
    let (widths, line, font_size) = with_font(size, |dc, _, line| {
        (lines.iter().map(|l| extent(dc, l)).collect::<Vec<_>>(), line, size)
    })?;
    // 右侧留一点余量容纳字形外伸。
    let width = widths.iter().copied().max().unwrap_or(0).max(1) + (font_size / 8).max(2);
    let height = line * lines.len() as i32;
    let mut mask = vec![0u8; (width * height) as usize];
    if text.chars().any(|c| !c.is_whitespace()) {
        let mut surface = super::pin::Surface::new(width, height)?;
        let font = font(font_size);
        ensure!(!font.is_invalid(), "创建字体失败");
        unsafe {
            let previous = SelectObject(surface.dc, HGDIOBJ(font.0));
            let area = RECT {
                left: 0,
                top: 0,
                right: width,
                bottom: height,
            };
            FillRect(surface.dc, &area, HBRUSH(GetStockObject(BLACK_BRUSH).0));
            SetBkMode(surface.dc, TRANSPARENT);
            SetTextColor(surface.dc, COLORREF(0xffffff));
            for (i, l) in lines.iter().enumerate() {
                let wide: Vec<u16> = l.encode_utf16().collect();
                let _ = TextOutW(surface.dc, 0, i as i32 * line, &wide);
            }
            SelectObject(surface.dc, previous);
            let _ = DeleteObject(HGDIOBJ(font.0));
            let _ = GdiFlush();
        }
        for (dst, px) in mask.iter_mut().zip(surface.bytes_mut().chunks_exact(4)) {
            *dst = px[1];
        }
    }
    Ok(Layout {
        width,
        height,
        line,
        mask,
    })
}

/// 光标（第 `caret` 个字符之前）相对文字左上角的位置。
pub(super) fn caret(text: &[char], caret: usize, size: i32) -> Result<(i32, i32)> {
    let before: String = text[..caret.min(text.len())].iter().collect();
    let row = before.matches('\n').count() as i32;
    let line_text = before.rsplit('\n').next().unwrap_or("");
    with_font(size, |dc, _, line| (extent(dc, line_text), row * line))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn layout_has_ink_lines_and_caret_moves_right_and_down() {
        let one = layout("Ag", 20).unwrap();
        assert!(one.width > 10 && one.height >= 20);
        assert!(one.mask.iter().any(|&v| v > 200));
        let two = layout("Ag\nAg", 20).unwrap();
        assert_eq!(two.height, one.line * 2);
        let empty = layout("", 20).unwrap();
        assert_eq!(empty.height, empty.line);
        assert!(empty.mask.iter().all(|&v| v == 0));
        let text: Vec<char> = "ab\ncd".chars().collect();
        let (x0, y0) = caret(&text, 0, 20).unwrap();
        let (x1, _) = caret(&text, 2, 20).unwrap();
        let (x2, y2) = caret(&text, 3, 20).unwrap();
        assert_eq!((x0, y0), (0, 0));
        assert!(x1 > 0);
        assert_eq!(x2, 0);
        assert!(y2 > 0);
    }
}
