//! 小型软件抗锯齿绘制：像素覆盖率、预乘 alpha 与 GDI 文字共存。
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;

pub struct Surface {
    pub dc: HDC,
    bitmap: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u32,
    pub width: i32,
    pub height: i32,
}

impl Surface {
    pub unsafe fn new(width: i32, height: i32) -> Option<Self> {
        if width <= 0 || height <= 0 {
            return None;
        }
        let dc = CreateCompatibleDC(None);
        if dc.0.is_null() {
            return None;
        }
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        match CreateDIBSection(Some(dc), &info, DIB_RGB_COLORS, &mut bits, None, 0) {
            Ok(bitmap) => {
                let old = SelectObject(dc, bitmap.into());
                Some(Self {
                    dc,
                    bitmap,
                    old,
                    bits: bits.cast(),
                    width,
                    height,
                })
            }
            Err(_) => {
                let _ = DeleteDC(dc);
                None
            }
        }
    }

    pub fn pixels(&self) -> &[u32] {
        unsafe { std::slice::from_raw_parts(self.bits, (self.width * self.height) as usize) }
    }

    pub fn pixels_mut(&mut self) -> &mut [u32] {
        unsafe { std::slice::from_raw_parts_mut(self.bits, (self.width * self.height) as usize) }
    }

    unsafe fn blend(&self, dest: HDC, x: i32, y: i32) {
        let _ = AlphaBlend(
            dest,
            x,
            y,
            self.width,
            self.height,
            self.dc,
            0,
            0,
            self.width,
            self.height,
            BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                BlendFlags: 0,
                SourceConstantAlpha: 255,
                AlphaFormat: AC_SRC_ALPHA as u8,
            },
        );
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            let _ = DeleteObject(self.bitmap.into());
            let _ = DeleteDC(self.dc);
        }
    }
}

/// 圆角矩形的有符号距离；负数位于内部。坐标以物理像素为单位。
pub fn round_distance(x: f32, y: f32, width: f32, height: f32, radius: f32) -> f32 {
    let radius = radius.min(width * 0.5).min(height * 0.5).max(0.0);
    let qx = (x - width * 0.5).abs() - (width * 0.5 - radius);
    let qy = (y - height * 0.5).abs() - (height * 0.5 - radius);
    qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0) - radius
}

fn coverage(distance: f32) -> f32 {
    (0.5 - distance).clamp(0.0, 1.0)
}

fn channels(color: COLORREF) -> [f32; 3] {
    [
        (color.0 & 255) as f32,
        ((color.0 >> 8) & 255) as f32,
        ((color.0 >> 16) & 255) as f32,
    ]
}

fn pack(rgb: [f32; 3], alpha: f32) -> u32 {
    let byte = |v: f32| v.round().clamp(0.0, 255.0) as u32;
    (byte(alpha * 255.0) << 24) | (byte(rgb[0]) << 16) | (byte(rgb[1]) << 8) | byte(rgb[2])
}

unsafe fn shape(hdc: HDC, r: RECT, color: COLORREF, distance: impl Fn(f32, f32) -> f32) {
    // 一像素外扩容纳完整的覆盖率渐变，避免形状被自己的位图边界截断。
    let width = r.right - r.left + 2;
    let height = r.bottom - r.top + 2;
    let Some(mut bitmap) = Surface::new(width, height) else {
        return;
    };
    let rgb = channels(color);
    for (i, pixel) in bitmap.pixels_mut().iter_mut().enumerate() {
        let x = (i % width as usize) as f32 - 0.5;
        let y = (i / width as usize) as f32 - 0.5;
        let alpha = coverage(distance(x, y));
        *pixel = pack(rgb.map(|c| c * alpha), alpha);
    }
    bitmap.blend(hdc, r.left - 1, r.top - 1);
}

pub unsafe fn rounded_fill(hdc: HDC, r: RECT, radius: f32, color: COLORREF) {
    shape(hdc, r, color, |x, y| {
        round_distance(
            x,
            y,
            (r.right - r.left) as f32,
            (r.bottom - r.top) as f32,
            radius,
        )
    });
}

pub unsafe fn rounded_stroke(hdc: HDC, r: RECT, radius: f32, thickness: f32, color: COLORREF) {
    shape(hdc, r, color, |x, y| {
        let d = round_distance(
            x,
            y,
            (r.right - r.left) as f32,
            (r.bottom - r.top) as f32,
            radius,
        );
        (d + thickness * 0.5).abs() - thickness * 0.5
    });
}

pub unsafe fn line(hdc: HDC, a: POINT, b: POINT, thickness: f32, color: COLORREF) {
    let pad = thickness.ceil() as i32 + 1;
    let r = RECT {
        left: a.x.min(b.x) - pad,
        top: a.y.min(b.y) - pad,
        right: a.x.max(b.x) + pad,
        bottom: a.y.max(b.y) + pad,
    };
    let ax = (a.x - r.left) as f32;
    let ay = (a.y - r.top) as f32;
    let dx = (b.x - a.x) as f32;
    let dy = (b.y - a.y) as f32;
    shape(hdc, r, color, |x, y| {
        let t = (((x - ax) * dx + (y - ay) * dy) / (dx * dx + dy * dy).max(0.001)).clamp(0.0, 1.0);
        (x - ax - t * dx).hypot(y - ay - t * dy) - thickness * 0.5
    });
}

/// GDI 对 alpha 通道没有稳定保证；仅取其 RGB，重新构建边框、圆角和柔和阴影。
pub fn compose(
    card: &Surface,
    output: &mut Surface,
    inset: i32,
    radius: f32,
    border: COLORREF,
    scale: f32,
) {
    let width = output.width as usize;
    let border = channels(border);
    let sigma = 4.0 * scale;
    let card_pixels = card.pixels();
    for (i, pixel) in output.pixels_mut().iter_mut().enumerate() {
        let x = (i % width) as f32 + 0.5 - inset as f32;
        let y = (i / width) as f32 + 0.5 - inset as f32;
        let d = round_distance(x, y, card.width as f32, card.height as f32, radius);
        let alpha = coverage(d);
        let shadow_d = round_distance(
            x,
            y - 3.0 * scale,
            card.width as f32,
            card.height as f32,
            radius,
        )
        .max(0.0);
        let shadow = 0.14 * (-shadow_d * shadow_d / (2.0 * sigma * sigma)).exp();
        let shadow = if shadow < 0.5 / 255.0 { 0.0 } else { shadow };
        let mut rgb = [0.0; 3];
        if alpha > 0.0 {
            let cx = (x.floor() as i32).clamp(0, card.width - 1) as usize;
            let cy = (y.floor() as i32).clamp(0, card.height - 1) as usize;
            let sample = card_pixels[cy * card.width as usize + cx];
            let inner = coverage(d + scale);
            let source = [
                ((sample >> 16) & 255) as f32,
                ((sample >> 8) & 255) as f32,
                (sample & 255) as f32,
            ];
            for c in 0..3 {
                rgb[c] = source[c] * inner + border[c] * (alpha - inner);
            }
        }
        let shadow_rgb = [25.0, 35.0, 50.0];
        for c in 0..3 {
            rgb[c] += shadow_rgb[c] * shadow * (1.0 - alpha);
        }
        *pixel = pack(rgb, alpha + shadow * (1.0 - alpha));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounded_edges_have_partial_coverage_and_symmetric_corners() {
        let a = coverage(round_distance(5.5, 4.5, 100.0, 60.0, 17.0));
        assert!(
            a > 0.0 && a < 1.0,
            "edge must contain fractional alpha: {a}"
        );
        assert!((a - coverage(round_distance(94.5, 4.5, 100.0, 60.0, 17.0))).abs() < 0.0001);
        assert_eq!(coverage(round_distance(0.5, 0.5, 100.0, 60.0, 17.0)), 0.0);
        assert_eq!(coverage(round_distance(50.5, 30.5, 100.0, 60.0, 17.0)), 1.0);
    }

    #[test]
    fn composed_surface_preserves_text_rgb_and_premultiplied_alpha() {
        unsafe {
            let mut card = Surface::new(100, 60).unwrap();
            card.pixels_mut().fill(0x001d1d1f); // 模拟 GDI 丢弃 alpha 的正文像素。
            let mut output = Surface::new(132, 92).unwrap();
            compose(&card, &mut output, 16, 17.0, COLORREF(0xece9e8), 1.0);
            assert_eq!(output.pixels()[46 * 132 + 66], 0xff1d1d1f);
            assert_eq!(output.pixels()[0] >> 24, 0);
            assert!(
                output
                    .pixels()
                    .iter()
                    .any(|p| (1..255).contains(&(p >> 24)))
            );
            for &p in output.pixels() {
                let a = p >> 24;
                assert!(((p >> 16) & 255) <= a && ((p >> 8) & 255) <= a && (p & 255) <= a);
            }
        }
    }
}
