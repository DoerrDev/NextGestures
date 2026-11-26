//! 剪贴板文字转贴图：交给系统 RichEdit 排版后画进位图。有 RTF 时保留字体、
//! 字号、颜色和粗斜体；只有纯文字时使用默认字体。
use anyhow::{Result, bail, ensure};
use std::sync::OnceLock;
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::LoadLibraryW;
use windows::Win32::UI::Controls::RichEdit::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

use super::pin::Surface;

/// 内容区最大宽度与四周留白（96 DPI 下的逻辑像素）。
const MAX_WIDTH: i32 = 720;
const PADDING: i32 = 12;
const MAX_HEIGHT: i32 = 16384;
const EM_EXLIMITTEXT: u32 = WM_USER + 53;

struct Editor(HWND);
impl Drop for Editor {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyWindow(self.0);
        }
    }
}

unsafe extern "system" fn read_rtf(cookie: usize, buffer: *mut u8, size: i32, read: *mut i32) -> u32 {
    unsafe {
        let source = &mut *(cookie as *mut &[u8]);
        let count = (size.max(0) as usize).min(source.len());
        std::ptr::copy_nonoverlapping(source.as_ptr(), buffer, count);
        *source = &source[count..];
        *read = count as i32;
    }
    0
}

fn editor(text: &str, rtf: Option<&[u8]>) -> Result<Editor> {
    static LOADED: OnceLock<bool> = OnceLock::new();
    ensure!(
        *LOADED.get_or_init(|| unsafe { LoadLibraryW(w!("Msftedit.dll")) }.is_ok()),
        "加载 RichEdit 失败"
    );
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            MSFTEDIT_CLASS,
            None,
            WS_POPUP | WINDOW_STYLE(ES_MULTILINE as u32),
            0,
            0,
            0,
            0,
            None,
            None,
            None,
            None,
        )?
    };
    let editor = Editor(hwnd);
    unsafe {
        SendMessageW(hwnd, EM_EXLIMITTEXT, None, Some(LPARAM(1 << 24)));
        // 纯文字的默认格式；RTF 自带的格式会覆盖它。
        let mut format = CHARFORMAT2W::default();
        format.Base.cbSize = size_of::<CHARFORMAT2W>() as u32;
        format.Base.dwMask = CFM_FACE | CFM_SIZE | CFM_COLOR;
        format.Base.yHeight = 11 * 20;
        format.Base.crTextColor = COLORREF(0x0033_2b26);
        let face: Vec<u16> = "Microsoft YaHei UI".encode_utf16().collect();
        format.Base.szFaceName[..face.len()].copy_from_slice(&face);
        SendMessageW(
            hwnd,
            EM_SETCHARFORMAT,
            Some(WPARAM(SCF_ALL as usize)),
            Some(LPARAM(&format as *const _ as isize)),
        );
        match rtf {
            Some(rtf) => {
                let mut source = rtf;
                let mut stream = EDITSTREAM {
                    dwCookie: &mut source as *mut &[u8] as usize,
                    dwError: 0,
                    pfnCallback: Some(read_rtf),
                };
                SendMessageW(
                    hwnd,
                    EM_STREAMIN,
                    Some(WPARAM(SF_RTF as usize)),
                    Some(LPARAM(&mut stream as *mut _ as isize)),
                );
                ensure!(stream.dwError == 0, "RTF 解析失败");
            }
            None => {
                let wide: Vec<u16> = text.encode_utf16().chain([0]).collect();
                SetWindowTextW(hwnd, PCWSTR(wide.as_ptr()))?;
            }
        }
    }
    Ok(editor)
}

/// 返回 (宽, 高, 不透明 BGRA)。先试 RTF，失败时退回纯文字。
pub(super) fn render(text: &str, rtf: Option<&[u8]>) -> Result<(i32, i32, Vec<u8>)> {
    if rtf.is_none() && text.trim().is_empty() {
        bail!("剪贴板文字为空");
    }
    if let Some(rtf) = rtf {
        match render_with(text, Some(rtf)) {
            Ok(image) => return Ok(image),
            Err(e) if !text.trim().is_empty() => log::warn!("RTF 贴图失败，改用纯文字: {e:#}"),
            Err(e) => return Err(e),
        }
    }
    render_with(text, None)
}

fn render_with(text: &str, rtf: Option<&[u8]>) -> Result<(i32, i32, Vec<u8>)> {
    let editor = editor(text, rtf)?;
    let probe = Surface::new(1, 1)?;
    let (dpi_x, dpi_y) = unsafe {
        (
            GetDeviceCaps(Some(probe.dc), LOGPIXELSX).max(96),
            GetDeviceCaps(Some(probe.dc), LOGPIXELSY).max(96),
        )
    };
    drop(probe);
    let px = |value: i32| value * dpi_x / 96;
    let (pad, max_width) = (px(PADDING), px(MAX_WIDTH));
    let twips_x = |pixels: i32| pixels * 1440 / dpi_x;
    let twips_y = |pixels: i32| pixels * 1440 / dpi_y;
    let max_height = MAX_HEIGHT - 2 * pad;

    // 第一遍只排版，量出实际高度。
    let mut surface = Surface::new(max_width + 2 * pad, 1)?;
    let mut range = FORMATRANGE {
        hdc: surface.dc,
        hdcTarget: surface.dc,
        rc: RECT {
            left: 0,
            top: 0,
            right: twips_x(max_width),
            bottom: twips_y(max_height),
        },
        rcPage: RECT {
            left: 0,
            top: 0,
            right: twips_x(max_width),
            bottom: twips_y(max_height),
        },
        chrg: CHARRANGE { cpMin: 0, cpMax: -1 },
    };
    unsafe {
        SendMessageW(
            editor.0,
            EM_FORMATRANGE,
            Some(WPARAM(0)),
            Some(LPARAM(&mut range as *mut _ as isize)),
        );
        SendMessageW(editor.0, EM_FORMATRANGE, Some(WPARAM(0)), Some(LPARAM(0)));
    }
    let height = ((range.rc.bottom * dpi_y + 1439) / 1440).clamp(0, max_height);
    if height == 0 {
        bail!("剪贴板文字为空");
    }

    // 第二遍画到白底上。
    let (width, full_height) = (max_width + 2 * pad, height + 2 * pad);
    surface = Surface::new(width, full_height)?;
    surface.bytes_mut().fill(255);
    let area = RECT {
        left: twips_x(pad),
        top: twips_y(pad),
        right: twips_x(pad + max_width),
        bottom: twips_y(pad + height),
    };
    range = FORMATRANGE {
        hdc: surface.dc,
        hdcTarget: surface.dc,
        rc: area,
        rcPage: RECT {
            left: 0,
            top: 0,
            right: twips_x(width),
            bottom: twips_y(full_height),
        },
        chrg: CHARRANGE { cpMin: 0, cpMax: -1 },
    };
    unsafe {
        SetBkMode(surface.dc, TRANSPARENT);
        SendMessageW(
            editor.0,
            EM_FORMATRANGE,
            Some(WPARAM(1)),
            Some(LPARAM(&mut range as *mut _ as isize)),
        );
        SendMessageW(editor.0, EM_FORMATRANGE, Some(WPARAM(0)), Some(LPARAM(0)));
        let _ = GdiFlush();
    }
    let bytes = surface.bytes_mut();
    // 短文字不必占满最大宽度：裁到最右侧有内容的列。
    let stride = width as usize * 4;
    let used = (0..width as usize)
        .rev()
        .find(|&x| {
            bytes
                .chunks_exact(stride)
                .any(|row| row[x * 4..x * 4 + 3].iter().any(|&c| c != 255))
        })
        .map_or(0, |x| x as i32 + 1);
    let cropped = (used + pad).clamp(pad * 2 + px(16), width);
    let mut pixels = Vec::with_capacity(cropped as usize * full_height as usize * 4);
    for row in bytes.chunks_exact(stride) {
        for pixel in row[..cropped as usize * 4].chunks_exact(4) {
            pixels.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 255]);
        }
    }
    Ok((cropped, full_height, pixels))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// (深色像素数, 红色像素数)
    fn ink(pixels: &[u8]) -> (usize, usize) {
        let dark = pixels
            .chunks_exact(4)
            .filter(|px| px[0] < 128 && px[1] < 128 && px[2] < 128)
            .count();
        let red = pixels
            .chunks_exact(4)
            .filter(|px| px[2] > 150 && px[0] < 90 && px[1] < 90)
            .count();
        (dark, red)
    }

    #[test]
    fn plain_text_renders_dark_glyphs_on_a_cropped_white_card() {
        let (width, height, pixels) = render("贴图 Hello\n第二行", None).unwrap();
        assert_eq!(pixels.len(), (width * height * 4) as usize);
        assert!(width < 720 && height > 30, "{width}x{height}");
        assert!(pixels.chunks_exact(4).all(|px| px[3] == 255));
        assert!(ink(&pixels).0 > 20);
        assert_eq!(&pixels[..4], &[255, 255, 255, 255]);
    }

    #[test]
    fn rtf_keeps_its_colors_and_long_text_wraps() {
        let rtf = br"{\rtf1\ansi{\colortbl;\red220\green30\blue30;}\cf1\b Red bold\b0\cf0  plain}";
        let (_, _, pixels) = render("Red bold plain", Some(rtf)).unwrap();
        assert!(ink(&pixels).1 > 10, "red glyph pixels missing");
        let long = "很长的一行文字 ".repeat(80);
        let (wrapped, tall, _) = render(&long, None).unwrap();
        assert!(wrapped <= 720 * 2 + 48 && tall > 60);
        assert!(render("", None).is_err());
    }
}
