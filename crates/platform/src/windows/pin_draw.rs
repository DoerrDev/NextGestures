//! 贴图涂鸦：形状模型与抗锯齿软件光栅化，直接写入 top-down BGRA 缓冲。
//! 所有坐标都是图片像素坐标；形状按顺序重放即可得到最终图片（撤销 = 弹出后重放）。

pub(super) type P = (f32, f32);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Tool {
    Rect,
    Ellipse,
    Arrow,
    Line,
    Pen,
    Marker,
    Mosaic,
    Text,
    Eraser,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Ink {
    Solid,
    /// 正片叠底，深色文字仍清晰可见。
    Marker,
    /// 以图片网格对齐的块大小。
    Mosaic(i32),
    /// 恢复原始截图像素。
    Erase,
}

#[derive(Clone, Debug)]
pub(super) enum Shape {
    /// 实心箭头：箭杆 + 三角箭尖，指向 `to`。
    Arrow {
        from: P,
        to: P,
        color: u32,
        width: f32,
    },
    Path {
        points: Vec<P>,
        closed: bool,
        ink: Ink,
        /// COLORREF：0x00BBGGRR
        color: u32,
        width: f32,
    },
    Text {
        left: i32,
        top: i32,
        width: i32,
        height: i32,
        mask: Vec<u8>,
        color: u32,
        /// 原文与逻辑字号，供再次编辑。
        text: String,
        size: f32,
    },
}

/// 图片尺寸的画布，可指向独立缓冲或贴图 DIB 的内容区域。
pub(super) struct Canvas<'a> {
    pub data: &'a mut [u8],
    pub stride: usize,
    pub offset: usize,
    pub width: i32,
    pub height: i32,
}
impl Canvas<'_> {
    fn index(&self, x: i32, y: i32) -> usize {
        self.offset + y as usize * self.stride + x as usize * 4
    }
}

struct Mask {
    left: i32,
    top: i32,
    width: i32,
    height: i32,
    cov: Vec<u8>,
    /// 每行实际写入的 [min, max)，避免遍历整个包围盒。
    spans: Vec<(i32, i32)>,
}

pub(super) fn rect(a: P, b: P) -> Vec<P> {
    vec![a, (b.0, a.1), b, (a.0, b.1)]
}

pub(super) fn ellipse(a: P, b: P) -> Vec<P> {
    let (cx, cy) = ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0);
    let (rx, ry) = ((b.0 - a.0).abs() / 2.0, (b.1 - a.1).abs() / 2.0);
    let n = ((rx + ry) * 0.8).clamp(24.0, 400.0) as usize;
    (0..n)
        .map(|i| {
            let t = i as f32 / n as f32 * std::f32::consts::TAU;
            (cx + rx * t.cos(), cy + ry * t.sin())
        })
        .collect()
}

fn distance(p: P, a: P, b: P) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len2 = dx * dx + dy * dy;
    let t = if len2 <= f32::EPSILON {
        0.0
    } else {
        (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / len2).clamp(0.0, 1.0)
    };
    let (x, y) = (a.0 + dx * t - p.0, a.1 + dy * t - p.1);
    (x * x + y * y).sqrt()
}

/// 胶囊体（线段外扩 r）与水平线 y 的交集区间；胶囊体是凸集，因此只有一段。
fn row_span(a: P, b: P, r: f32, y: f32) -> Option<(f32, f32)> {
    let mut lo = f32::INFINITY;
    let mut hi = f32::NEG_INFINITY;
    for c in [a, b] {
        let dy = y - c.1;
        if dy.abs() <= r {
            let h = (r * r - dy * dy).sqrt();
            lo = lo.min(c.0 - h);
            hi = hi.max(c.0 + h);
        }
    }
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len = (dx * dx + dy * dy).sqrt();
    if len > f32::EPSILON {
        let (nx, ny) = (-dy / len * r, dx / len * r);
        let quad = [
            (a.0 + nx, a.1 + ny),
            (b.0 + nx, b.1 + ny),
            (b.0 - nx, b.1 - ny),
            (a.0 - nx, a.1 - ny),
        ];
        for i in 0..4 {
            let (p, q) = (quad[i], quad[(i + 1) % 4]);
            if (p.1 <= y && y <= q.1) || (q.1 <= y && y <= p.1) {
                if (q.1 - p.1).abs() <= f32::EPSILON {
                    lo = lo.min(p.0.min(q.0));
                    hi = hi.max(p.0.max(q.0));
                } else {
                    let x = p.0 + (y - p.1) / (q.1 - p.1) * (q.0 - p.0);
                    lo = lo.min(x);
                    hi = hi.max(x);
                }
            }
        }
    }
    (lo <= hi).then_some((lo, hi))
}

fn stroke_mask(points: &[P], closed: bool, width: f32, cw: i32, ch: i32) -> Option<Mask> {
    let first = *points.first()?;
    let mut segments: Vec<(P, P)> = points.windows(2).map(|s| (s[0], s[1])).collect();
    if points.len() == 1 {
        segments.push((first, first));
    } else if closed && points.len() > 2 {
        segments.push((*points.last().unwrap(), first));
    }
    segment_mask(points, &segments, width, cw, ch)
}

/// 以 `extent` 的包围盒建遮罩，再把各线段按圆头胶囊体累加进去。
fn segment_mask(extent: &[P], segments: &[(P, P)], width: f32, cw: i32, ch: i32) -> Option<Mask> {
    let first = *extent.first()?;
    let r = width / 2.0;
    let pad = r + 1.0;
    let (mut x0, mut y0, mut x1, mut y1) = (first.0, first.1, first.0, first.1);
    for p in extent {
        x0 = x0.min(p.0);
        y0 = y0.min(p.1);
        x1 = x1.max(p.0);
        y1 = y1.max(p.1);
    }
    let left = ((x0 - pad).floor() as i32).max(0);
    let top = ((y0 - pad).floor() as i32).max(0);
    let right = ((x1 + pad).ceil() as i32).min(cw);
    let bottom = ((y1 + pad).ceil() as i32).min(ch);
    if left >= right || top >= bottom {
        return None;
    }
    let (w, h) = (right - left, bottom - top);
    let mut mask = Mask {
        left,
        top,
        width: w,
        height: h,
        cov: vec![0; w as usize * h as usize],
        spans: vec![(i32::MAX, i32::MIN); h as usize],
    };
    let reach = r + 0.5;
    for &(a, b) in segments {
        let sy0 = ((a.1.min(b.1) - pad).floor() as i32).max(top);
        let sy1 = ((a.1.max(b.1) + pad).ceil() as i32).min(bottom);
        for y in sy0..sy1 {
            let cy = y as f32 + 0.5;
            let Some((lo, hi)) = row_span(a, b, reach, cy) else {
                continue;
            };
            let sx0 = ((lo - 0.5).floor() as i32).max(left);
            let sx1 = ((hi + 0.5).ceil() as i32 + 1).min(right);
            if sx0 >= sx1 {
                continue;
            }
            let row = (y - top) as usize;
            let span = &mut mask.spans[row];
            span.0 = span.0.min(sx0);
            span.1 = span.1.max(sx1);
            for x in sx0..sx1 {
                let c = (reach - distance((x as f32 + 0.5, cy), a, b)).clamp(0.0, 1.0);
                if c > 0.0 {
                    let i = row * w as usize + (x - left) as usize;
                    let v = (c * 255.0).round() as u8;
                    if v > mask.cov[i] {
                        mask.cov[i] = v;
                    }
                }
            }
        }
    }
    Some(mask)
}

/// 箭头几何：(箭杆终点, [箭尖, 左翼, 右翼])。箭头大小随线宽增长，短线时自动缩小。
pub(super) fn arrow_geometry(from: P, to: P, width: f32) -> (P, [P; 3]) {
    let (dx, dy) = (to.0 - from.0, to.1 - from.1);
    let len = (dx * dx + dy * dy).sqrt().max(f32::EPSILON);
    let (ux, uy) = (dx / len, dy / len);
    let head = (width * 3.0 + 7.0).min(len);
    let half = head * 0.55;
    let back = (to.0 - ux * head, to.1 - uy * head);
    let base = (to.0 - ux * head * 0.6, to.1 - uy * head * 0.6);
    (
        base,
        [
            to,
            (back.0 - uy * half, back.1 + ux * half),
            (back.0 + uy * half, back.1 - ux * half),
        ],
    )
}

fn arrow_mask(from: P, to: P, width: f32, cw: i32, ch: i32) -> Option<Mask> {
    let (base, tri) = arrow_geometry(from, to, width);
    let mut mask = segment_mask(&[from, tri[0], tri[1], tri[2]], &[(from, base)], width, cw, ch)?;
    // 凸三角形的有符号距离取各边距离最大值，0.5 像素过渡抗锯齿。
    let edges: Vec<(P, f32, f32, f32)> = (0..3)
        .map(|i| {
            let (p, q, other) = (tri[i], tri[(i + 1) % 3], tri[(i + 2) % 3]);
            let (nx, ny) = (q.1 - p.1, p.0 - q.0);
            let n = (nx * nx + ny * ny).sqrt().max(f32::EPSILON);
            let (nx, ny) = (nx / n, ny / n);
            let sign = if (other.0 - p.0) * nx + (other.1 - p.1) * ny > 0.0 { -1.0 } else { 1.0 };
            (p, nx * sign, ny * sign, 0.0)
        })
        .collect();
    let xs = tri.map(|p| p.0);
    let ys = tri.map(|p| p.1);
    let x0 = (xs.iter().copied().fold(f32::INFINITY, f32::min).floor() as i32 - 1).max(mask.left);
    let x1 = (xs.iter().copied().fold(f32::NEG_INFINITY, f32::max).ceil() as i32 + 1).min(mask.left + mask.width);
    let y0 = (ys.iter().copied().fold(f32::INFINITY, f32::min).floor() as i32 - 1).max(mask.top);
    let y1 = (ys.iter().copied().fold(f32::NEG_INFINITY, f32::max).ceil() as i32 + 1).min(mask.top + mask.height);
    for y in y0..y1 {
        let row = (y - mask.top) as usize;
        for x in x0..x1 {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let d = edges
                .iter()
                .map(|(p, nx, ny, _)| (px - p.0) * nx + (py - p.1) * ny)
                .fold(f32::NEG_INFINITY, f32::max);
            let c = (0.5 - d).clamp(0.0, 1.0);
            if c > 0.0 {
                let i = row * mask.width as usize + (x - mask.left) as usize;
                mask.cov[i] = mask.cov[i].max((c * 255.0).round() as u8);
                let span = &mut mask.spans[row];
                span.0 = span.0.min(x);
                span.1 = span.1.max(x + 1);
            }
        }
    }
    Some(mask)
}

/// 多段折线描边的覆盖率图（w×h），供工具栏图标等 UI 元素抗锯齿绘制。
pub(super) fn stroke_coverage(paths: &[(Vec<P>, bool)], width: f32, w: i32, h: i32) -> Vec<u8> {
    let mut cov = vec![0u8; w.max(0) as usize * h.max(0) as usize];
    for (points, closed) in paths {
        let Some(mask) = stroke_mask(points, *closed, width, w, h) else {
            continue;
        };
        for row in 0..mask.height {
            let (x0, x1) = mask.spans[row as usize];
            for x in x0..x1 {
                let v = mask.cov[(row * mask.width + x - mask.left) as usize];
                let i = ((mask.top + row) * w + x) as usize;
                cov[i] = cov[i].max(v);
            }
        }
    }
    cov
}

fn bgr(color: u32) -> [u8; 3] {
    [(color >> 16) as u8, (color >> 8) as u8, color as u8]
}

fn mix(dst: u8, target: u8, cov: u32) -> u8 {
    ((u32::from(dst) * (255 - cov) + u32::from(target) * cov + 127) / 255) as u8
}

fn apply(canvas: &mut Canvas, mask: &Mask, ink: Ink, color: u32, base: &[u8]) {
    let tint = bgr(color);
    // 记号笔调淡颜色后正片叠底，避免纯色把底图压得过暗。
    let marker = tint.map(|c| 255 - ((255 - u32::from(c)) * 3 / 5) as u8);
    // 马赛克块均值按需计算：块内像素只会在该块均值算出之后才被改写。
    let mut blocks: Vec<Option<[u8; 3]>> = Vec::new();
    let (mut bx0, mut by0, mut bcols) = (0, 0, 0);
    if let Ink::Mosaic(size) = ink {
        let size = size.max(2);
        bx0 = mask.left / size;
        by0 = mask.top / size;
        let bx1 = (mask.left + mask.width - 1) / size;
        let by1 = (mask.top + mask.height - 1) / size;
        bcols = bx1 - bx0 + 1;
        blocks = vec![None; (bcols * (by1 - by0 + 1)) as usize];
    }
    let average = |canvas: &Canvas, bx: i32, by: i32, size: i32| {
        let mut sum = [0u64; 3];
        let mut n = 0u64;
        for y in by * size..((by + 1) * size).min(canvas.height) {
            for x in bx * size..((bx + 1) * size).min(canvas.width) {
                let i = canvas.index(x, y);
                for c in 0..3 {
                    sum[c] += u64::from(canvas.data[i + c]);
                }
                n += 1;
            }
        }
        sum.map(|s| (s / n.max(1)) as u8)
    };
    for row in 0..mask.height {
        let (sx0, sx1) = mask.spans[row as usize];
        let y = mask.top + row;
        for x in sx0..sx1 {
            let cov = u32::from(mask.cov[row as usize * mask.width as usize + (x - mask.left) as usize]);
            if cov == 0 {
                continue;
            }
            let block = if let Ink::Mosaic(size) = ink {
                let size = size.max(2);
                let slot = ((y / size - by0) * bcols + (x / size - bx0)) as usize;
                *blocks[slot].get_or_insert_with(|| average(canvas, x / size, y / size, size))
            } else {
                [0; 3]
            };
            let i = canvas.index(x, y);
            let px = &mut canvas.data[i..i + 4];
            let target = match ink {
                Ink::Solid => tint,
                Ink::Marker => [0, 1, 2].map(|c| (u32::from(px[c]) * u32::from(marker[c]) / 255) as u8),
                Ink::Erase => {
                    let j = (y as usize * canvas.width as usize + x as usize) * 4;
                    [base[j], base[j + 1], base[j + 2]]
                }
                Ink::Mosaic(_) => block,
            };
            for c in 0..3 {
                px[c] = mix(px[c], target[c], cov);
            }
            px[3] = 255;
        }
    }
}

/// 把形状画到画布；`base` 是与画布同尺寸的原始截图，供橡皮擦恢复。
pub(super) fn draw(shape: &Shape, canvas: &mut Canvas, base: &[u8]) {
    match shape {
        Shape::Arrow {
            from,
            to,
            color,
            width,
        } => {
            if let Some(mask) = arrow_mask(*from, *to, *width, canvas.width, canvas.height) {
                apply(canvas, &mask, Ink::Solid, *color, base);
            }
        }
        Shape::Path {
            points,
            closed,
            ink,
            color,
            width,
        } => {
            if let Some(mask) = stroke_mask(points, *closed, *width, canvas.width, canvas.height) {
                apply(canvas, &mask, *ink, *color, base);
            }
        }
        Shape::Text {
            left,
            top,
            width,
            height,
            mask,
            color,
            ..
        } => {
            let x0 = (*left).max(0);
            let y0 = (*top).max(0);
            let x1 = (left + width).min(canvas.width);
            let y1 = (top + height).min(canvas.height);
            if x0 >= x1 || y0 >= y1 {
                return;
            }
            let (w, h) = (x1 - x0, y1 - y0);
            let mut cov = Vec::with_capacity(w as usize * h as usize);
            for y in y0..y1 {
                let row = ((y - top) * width) as usize;
                cov.extend_from_slice(&mask[row + (x0 - left) as usize..row + (x1 - left) as usize]);
            }
            let clipped = Mask {
                left: x0,
                top: y0,
                width: w,
                height: h,
                cov,
                spans: vec![(x0, x1); h as usize],
            };
            apply(canvas, &clipped, Ink::Solid, *color, base);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(w: i32, h: i32, bgr: [u8; 3]) -> Vec<u8> {
        (0..w * h)
            .flat_map(|_| [bgr[0], bgr[1], bgr[2], 255])
            .collect()
    }
    fn at(data: &[u8], w: i32, x: i32, y: i32) -> [u8; 4] {
        let i = ((y * w + x) * 4) as usize;
        [data[i], data[i + 1], data[i + 2], data[i + 3]]
    }
    fn paint(data: &mut Vec<u8>, w: i32, h: i32, shape: &Shape, base: &[u8]) {
        let mut canvas = Canvas {
            data,
            stride: w as usize * 4,
            offset: 0,
            width: w,
            height: h,
        };
        draw(shape, &mut canvas, base);
    }

    #[test]
    fn solid_line_covers_its_center_and_leaves_far_pixels() {
        let base = image(40, 20, [255, 255, 255]);
        let mut data = base.clone();
        let shape = Shape::Path {
            points: vec![(5.5, 10.0), (35.5, 10.0)],
            closed: false,
            ink: Ink::Solid,
            color: 0x0000ff, // 红
            width: 3.0,
        };
        paint(&mut data, 40, 20, &shape, &base);
        assert_eq!(at(&data, 40, 20, 10), [0, 0, 255, 255]);
        assert_eq!(at(&data, 40, 20, 2), [255, 255, 255, 255]);
        let edge = at(&data, 40, 20, 11);
        assert!(edge[0] > 0 && edge[0] < 255, "anti-aliased edge {edge:?}");
    }

    #[test]
    fn eraser_restores_original_and_rect_is_hollow() {
        let base = image(40, 40, [10, 20, 30]);
        let mut data = base.clone();
        let rect = Shape::Path {
            points: rect((5.5, 5.5), (34.5, 34.5)),
            closed: true,
            ink: Ink::Solid,
            color: 0xffffff,
            width: 3.0,
        };
        paint(&mut data, 40, 40, &rect, &base);
        assert_eq!(at(&data, 40, 20, 5), [255, 255, 255, 255]);
        assert_eq!(at(&data, 40, 20, 20), [10, 20, 30, 255]);
        let erase = Shape::Path {
            points: vec![(20.5, 0.5), (20.5, 39.5)],
            closed: false,
            ink: Ink::Erase,
            color: 0,
            width: 6.0,
        };
        paint(&mut data, 40, 40, &erase, &base);
        assert_eq!(at(&data, 40, 20, 5), [10, 20, 30, 255]);
        assert_eq!(at(&data, 40, 5, 20), [255, 255, 255, 255]);
    }

    #[test]
    fn mosaic_averages_aligned_blocks_and_marker_multiplies() {
        let w = 20;
        let mut base = image(w, 10, [0, 0, 0]);
        for y in 0..10 {
            for x in (0..w).step_by(2) {
                let i = ((y * w + x) * 4) as usize;
                base[i..i + 3].copy_from_slice(&[200, 200, 200]);
            }
        }
        let mut data = base.clone();
        let mosaic = Shape::Path {
            points: vec![(0.5, 5.0), (19.5, 5.0)],
            closed: false,
            ink: Ink::Mosaic(10),
            color: 0,
            width: 30.0,
        };
        paint(&mut data, w, 10, &mosaic, &base);
        assert_eq!(at(&data, w, 3, 3), [100, 100, 100, 255]);
        assert_eq!(at(&data, w, 4, 3), [100, 100, 100, 255]);

        let mut white = image(10, 10, [255, 255, 255]);
        let mut black = image(10, 10, [0, 0, 0]);
        let marker = Shape::Path {
            points: vec![(0.5, 5.0), (9.5, 5.0)],
            closed: false,
            ink: Ink::Marker,
            color: 0x00ffff, // 黄
            width: 6.0,
        };
        let b = white.clone();
        paint(&mut white, 10, 10, &marker, &b);
        paint(&mut black, 10, 10, &marker, &b);
        let tinted = at(&white, 10, 5, 5);
        assert!(tinted[0] < 255 && tinted[1] == 255 && tinted[2] == 255);
        assert_eq!(at(&black, 10, 5, 5), [0, 0, 0, 255]);
    }

    #[test]
    fn arrow_head_is_filled_and_wider_than_the_shaft() {
        let base = image(60, 30, [255, 255, 255]);
        let mut data = base.clone();
        let arrow = Shape::Arrow {
            from: (5.0, 15.0),
            to: (55.0, 15.0),
            color: 0x000000,
            width: 3.0,
        };
        paint(&mut data, 60, 30, &arrow, &base);
        // 箭杆中部只有线宽，箭头根部明显更宽，箭尖附近仍有墨迹。
        assert_eq!(at(&data, 60, 20, 15)[0], 0);
        assert_eq!(at(&data, 60, 20, 19)[0], 255);
        assert!(at(&data, 60, 46, 19)[0] < 128);
        assert!(at(&data, 60, 53, 15)[0] < 128);
        assert_eq!(at(&data, 60, 57, 15)[0], 255);
    }

    #[test]
    fn text_mask_is_clipped_to_canvas() {
        let base = image(10, 10, [0, 0, 0]);
        let mut data = base.clone();
        let text = Shape::Text {
            left: 8,
            top: -1,
            width: 4,
            height: 3,
            mask: vec![255; 12],
            color: 0xffffff,
            text: String::new(),
            size: 20.0,
        };
        paint(&mut data, 10, 10, &text, &base);
        assert_eq!(at(&data, 10, 9, 0), [255, 255, 255, 255]);
        assert_eq!(at(&data, 10, 9, 1), [255, 255, 255, 255]);
        assert_eq!(at(&data, 10, 9, 2), [0, 0, 0, 255]);
        assert_eq!(at(&data, 10, 7, 0), [0, 0, 0, 255]);
    }
}
