//! 内嵌 lucide 图标（lucide-static 1.35.0，ISC 许可，见 assets/lucide），
//! 解析为 24×24 坐标系下的折线，交给涂鸦光栅器按圆头圆角描边。
use super::pin_draw::P;

pub(super) const SQUARE: &str = include_str!("../../assets/lucide/square.svg");
pub(super) const CIRCLE: &str = include_str!("../../assets/lucide/circle.svg");
pub(super) const ARROW: &str = include_str!("../../assets/lucide/move-up-right.svg");
pub(super) const SLASH: &str = include_str!("../../assets/lucide/slash.svg");
pub(super) const PENCIL: &str = include_str!("../../assets/lucide/pencil.svg");
pub(super) const HIGHLIGHTER: &str = include_str!("../../assets/lucide/highlighter.svg");
pub(super) const GRID: &str = include_str!("../../assets/lucide/grid-3x3.svg");
pub(super) const TYPE: &str = include_str!("../../assets/lucide/type.svg");
pub(super) const ERASER: &str = include_str!("../../assets/lucide/eraser.svg");
pub(super) const UNDO: &str = include_str!("../../assets/lucide/undo-2.svg");
pub(super) const COPY: &str = include_str!("../../assets/lucide/copy.svg");
pub(super) const SAVE: &str = include_str!("../../assets/lucide/save.svg");
pub(super) const X: &str = include_str!("../../assets/lucide/x.svg");
pub(super) const PALETTE: &str = include_str!("../../assets/lucide/palette.svg");
pub(super) const MINUS: &str = include_str!("../../assets/lucide/minus.svg");
pub(super) const PLUS: &str = include_str!("../../assets/lucide/plus.svg");
pub(super) const CHECK: &str = include_str!("../../assets/lucide/check.svg");

pub(super) type Outline = Vec<(Vec<P>, bool)>;

fn attribute<'a>(tag: &'a str, key: &str) -> Option<&'a str> {
    let pattern = format!("{key}=\"");
    let mut from = 0;
    while let Some(found) = tag[from..].find(&pattern) {
        let at = from + found;
        if at > 0 && tag.as_bytes()[at - 1].is_ascii_whitespace() {
            let value = &tag[at + pattern.len()..];
            return value.find('"').map(|end| &value[..end]);
        }
        from = at + pattern.len();
    }
    None
}

fn ellipse(cx: f32, cy: f32, rx: f32, ry: f32) -> Vec<P> {
    let n = 32;
    (0..n)
        .map(|i| {
            let t = i as f32 / n as f32 * std::f32::consts::TAU;
            (cx + rx * t.cos(), cy + ry * t.sin())
        })
        .collect()
}

fn rounded_rect(x: f32, y: f32, w: f32, h: f32, rx: f32, ry: f32) -> Vec<P> {
    let (rx, ry) = (rx.min(w / 2.0), ry.min(h / 2.0));
    if rx <= 0.0 || ry <= 0.0 {
        return vec![(x, y), (x + w, y), (x + w, y + h), (x, y + h)];
    }
    let corners = [
        (x + w - rx, y + ry, -90.0f32),
        (x + w - rx, y + h - ry, 0.0),
        (x + rx, y + h - ry, 90.0),
        (x + rx, y + ry, 180.0),
    ];
    let mut points = Vec::new();
    for (cx, cy, start) in corners {
        for i in 0..=6 {
            let t = (start + i as f32 * 15.0).to_radians();
            points.push((cx + rx * t.cos(), cy + ry * t.sin()));
        }
    }
    points
}

/// 解析 SVG 中的 path / circle / ellipse / rect / line / polyline / polygon。
pub(super) fn outline(svg: &str) -> Outline {
    let mut out = Vec::new();
    let mut rest = svg;
    while let Some(start) = rest.find('<') {
        rest = &rest[start + 1..];
        let end = rest.find('>').unwrap_or(rest.len());
        let tag = &rest[..end];
        rest = &rest[end..];
        let name = tag.split_whitespace().next().unwrap_or("");
        let num = |key: &str| attribute(tag, key).and_then(|v| v.trim().parse::<f32>().ok());
        match name {
            "path" => {
                if let Some(d) = attribute(tag, "d") {
                    path(d, &mut out);
                }
            }
            "circle" => {
                let r = num("r").unwrap_or(0.0);
                out.push((ellipse(num("cx").unwrap_or(0.0), num("cy").unwrap_or(0.0), r, r), true));
            }
            "ellipse" => out.push((
                ellipse(
                    num("cx").unwrap_or(0.0),
                    num("cy").unwrap_or(0.0),
                    num("rx").unwrap_or(0.0),
                    num("ry").unwrap_or(0.0),
                ),
                true,
            )),
            "rect" => {
                let rx = num("rx").or(num("ry")).unwrap_or(0.0);
                out.push((
                    rounded_rect(
                        num("x").unwrap_or(0.0),
                        num("y").unwrap_or(0.0),
                        num("width").unwrap_or(0.0),
                        num("height").unwrap_or(0.0),
                        rx,
                        num("ry").unwrap_or(rx),
                    ),
                    true,
                ));
            }
            "line" => out.push((
                vec![
                    (num("x1").unwrap_or(0.0), num("y1").unwrap_or(0.0)),
                    (num("x2").unwrap_or(0.0), num("y2").unwrap_or(0.0)),
                ],
                false,
            )),
            "polyline" | "polygon" => {
                if let Some(points) = attribute(tag, "points") {
                    let mut lexer = Lexer::new(points);
                    let mut list = Vec::new();
                    while lexer.has_number() {
                        let x = lexer.number();
                        let y = lexer.number();
                        list.push((x, y));
                    }
                    out.push((list, name == "polygon"));
                }
            }
            _ => {}
        }
    }
    out
}

struct Lexer<'a> {
    s: &'a [u8],
    i: usize,
}
impl<'a> Lexer<'a> {
    fn new(s: &'a str) -> Self {
        Self { s: s.as_bytes(), i: 0 }
    }
    fn skip(&mut self) {
        while self.i < self.s.len() && (self.s[self.i].is_ascii_whitespace() || self.s[self.i] == b',') {
            self.i += 1;
        }
    }
    fn command(&mut self) -> Option<u8> {
        self.skip();
        let c = *self.s.get(self.i)?;
        (c.is_ascii_alphabetic() && c != b'e' && c != b'E').then(|| {
            self.i += 1;
            c
        })
    }
    fn has_number(&mut self) -> bool {
        self.skip();
        self.s
            .get(self.i)
            .is_some_and(|c| c.is_ascii_digit() || matches!(c, b'-' | b'+' | b'.'))
    }
    fn number(&mut self) -> f32 {
        self.skip();
        let start = self.i;
        if matches!(self.s.get(self.i), Some(b'-' | b'+')) {
            self.i += 1;
        }
        let mut dot = false;
        while let Some(&c) = self.s.get(self.i) {
            if c.is_ascii_digit() {
                self.i += 1;
            } else if c == b'.' && !dot {
                dot = true;
                self.i += 1;
            } else if (c == b'e' || c == b'E')
                && self
                    .s
                    .get(self.i + 1)
                    .is_some_and(|n| n.is_ascii_digit() || matches!(n, b'-' | b'+'))
            {
                self.i += 2;
                while self.s.get(self.i).is_some_and(|c| c.is_ascii_digit()) {
                    self.i += 1;
                }
                break;
            } else {
                break;
            }
        }
        std::str::from_utf8(&self.s[start..self.i])
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0.0)
    }
    /// 弧线标志位可能与后续数字紧挨（如 "01"），只读一个字符。
    fn flag(&mut self) -> bool {
        self.skip();
        let c = self.s.get(self.i).copied();
        self.i += 1;
        c == Some(b'1')
    }
}

fn cubic(p0: P, c1: P, c2: P, p1: P, out: &mut Vec<P>) {
    for i in 1..=12 {
        let t = i as f32 / 12.0;
        let u = 1.0 - t;
        let (a, b, c, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
        out.push((
            a * p0.0 + b * c1.0 + c * c2.0 + d * p1.0,
            a * p0.1 + b * c1.1 + c * c2.1 + d * p1.1,
        ));
    }
}

fn quadratic(p0: P, c: P, p1: P, out: &mut Vec<P>) {
    for i in 1..=10 {
        let t = i as f32 / 10.0;
        let u = 1.0 - t;
        out.push((
            u * u * p0.0 + 2.0 * u * t * c.0 + t * t * p1.0,
            u * u * p0.1 + 2.0 * u * t * c.1 + t * t * p1.1,
        ));
    }
}

/// SVG 规范 F.6.5：端点参数化转换为中心参数化后按角度细分。
#[allow(clippy::too_many_arguments)]
fn arc(p0: P, rx: f32, ry: f32, rotation: f32, large: bool, sweep: bool, p1: P, out: &mut Vec<P>) {
    if p0 == p1 {
        return;
    }
    let (mut rx, mut ry) = (rx.abs(), ry.abs());
    if rx <= f32::EPSILON || ry <= f32::EPSILON {
        out.push(p1);
        return;
    }
    let (s, c) = rotation.to_radians().sin_cos();
    let (dx, dy) = ((p0.0 - p1.0) / 2.0, (p0.1 - p1.1) / 2.0);
    let (x1, y1) = (c * dx + s * dy, -s * dx + c * dy);
    let lambda = x1 * x1 / (rx * rx) + y1 * y1 / (ry * ry);
    if lambda > 1.0 {
        rx *= lambda.sqrt();
        ry *= lambda.sqrt();
    }
    let num = rx * rx * ry * ry - rx * rx * y1 * y1 - ry * ry * x1 * x1;
    let den = rx * rx * y1 * y1 + ry * ry * x1 * x1;
    let coef = (num / den).max(0.0).sqrt() * if large == sweep { -1.0 } else { 1.0 };
    let (cxp, cyp) = (coef * rx * y1 / ry, -coef * ry * x1 / rx);
    let (cx, cy) = (
        c * cxp - s * cyp + (p0.0 + p1.0) / 2.0,
        s * cxp + c * cyp + (p0.1 + p1.1) / 2.0,
    );
    let angle = |u: P, v: P| (u.0 * v.1 - u.1 * v.0).atan2(u.0 * v.0 + u.1 * v.1);
    let u = ((x1 - cxp) / rx, (y1 - cyp) / ry);
    let v = ((-x1 - cxp) / rx, (-y1 - cyp) / ry);
    let start = angle((1.0, 0.0), u);
    let mut delta = angle(u, v);
    if !sweep && delta > 0.0 {
        delta -= std::f32::consts::TAU;
    } else if sweep && delta < 0.0 {
        delta += std::f32::consts::TAU;
    }
    let n = (delta.abs() / (std::f32::consts::PI / 16.0)).ceil().max(2.0) as usize;
    for i in 1..=n {
        let t = start + delta * i as f32 / n as f32;
        let (st, ct) = t.sin_cos();
        out.push((cx + rx * ct * c - ry * st * s, cy + rx * ct * s + ry * st * c));
    }
    if let Some(last) = out.last_mut() {
        *last = p1;
    }
}

fn path(d: &str, out: &mut Outline) {
    let mut lexer = Lexer::new(d);
    let mut sub: Vec<P> = Vec::new();
    let (mut cur, mut start) = ((0.0f32, 0.0f32), (0.0f32, 0.0f32));
    let mut previous = 0u8;
    let mut control: Option<P> = None;
    let flush = |sub: &mut Vec<P>, closed: bool, out: &mut Outline| {
        if sub.len() > 1 {
            out.push((std::mem::take(sub), closed));
        } else {
            sub.clear();
        }
    };
    loop {
        let cmd = match lexer.command() {
            Some(c) => c,
            None if lexer.has_number() && previous != 0 => match previous {
                b'M' => b'L',
                b'm' => b'l',
                c => c,
            },
            None => break,
        };
        let rel = cmd.is_ascii_lowercase();
        let origin = if rel { cur } else { (0.0, 0.0) };
        let point = |lexer: &mut Lexer, origin: P| {
            let x = lexer.number();
            let y = lexer.number();
            (origin.0 + x, origin.1 + y)
        };
        if sub.is_empty() && !matches!(cmd, b'M' | b'm' | b'Z' | b'z') {
            sub.push(cur);
        }
        let mut next_control = None;
        match cmd.to_ascii_uppercase() {
            b'M' => {
                flush(&mut sub, false, out);
                cur = point(&mut lexer, origin);
                start = cur;
                sub.push(cur);
            }
            b'L' => {
                cur = point(&mut lexer, origin);
                sub.push(cur);
            }
            b'H' => {
                cur.0 = origin.0 + lexer.number();
                sub.push(cur);
            }
            b'V' => {
                cur.1 = origin.1 + lexer.number();
                sub.push(cur);
            }
            b'C' | b'S' => {
                let c1 = if cmd.to_ascii_uppercase() == b'C' {
                    point(&mut lexer, origin)
                } else {
                    control.map_or(cur, |c| (2.0 * cur.0 - c.0, 2.0 * cur.1 - c.1))
                };
                let c2 = point(&mut lexer, origin);
                let p = point(&mut lexer, origin);
                cubic(cur, c1, c2, p, &mut sub);
                next_control = Some(c2);
                cur = p;
            }
            b'Q' | b'T' => {
                let c = if cmd.to_ascii_uppercase() == b'Q' {
                    point(&mut lexer, origin)
                } else {
                    control.map_or(cur, |c| (2.0 * cur.0 - c.0, 2.0 * cur.1 - c.1))
                };
                let p = point(&mut lexer, origin);
                quadratic(cur, c, p, &mut sub);
                next_control = Some(c);
                cur = p;
            }
            b'A' => {
                let rx = lexer.number();
                let ry = lexer.number();
                let rotation = lexer.number();
                let large = lexer.flag();
                let sweep = lexer.flag();
                let p = point(&mut lexer, origin);
                arc(cur, rx, ry, rotation, large, sweep, p, &mut sub);
                cur = p;
            }
            b'Z' => {
                flush(&mut sub, true, out);
                cur = start;
            }
            _ => break,
        }
        control = next_control;
        previous = cmd;
    }
    flush(&mut sub, false, out);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_every_embedded_icon_into_strokes_inside_the_view_box() {
        for svg in [
            SQUARE, CIRCLE, ARROW, SLASH, PENCIL, HIGHLIGHTER, GRID, TYPE, ERASER, UNDO, COPY, SAVE,
            X, PALETTE, MINUS, PLUS, CHECK,
        ] {
            let outline = outline(svg);
            assert!(!outline.is_empty(), "{svg}");
            for (points, _) in &outline {
                assert!(points.len() >= 2);
                for &(x, y) in points {
                    assert!((-0.5..=24.5).contains(&x) && (-0.5..=24.5).contains(&y), "{x},{y} in {svg}");
                }
            }
        }
    }
    #[test]
    fn relative_lines_arcs_and_close_follow_svg_rules() {
        let mut out = Vec::new();
        path("M2 2h4v4H2z m10 0 2 2", &mut out);
        assert_eq!(out[0], (vec![(2.0, 2.0), (6.0, 2.0), (6.0, 6.0), (2.0, 6.0)], true));
        assert_eq!(out[1], (vec![(12.0, 2.0), (14.0, 4.0)], false));
        let mut out = Vec::new();
        path("M0 10a10 10 0 0 1 20 0", &mut out);
        let top = out[0].0.iter().map(|p| p.1).fold(f32::INFINITY, f32::min);
        assert!((top - 0.0).abs() < 0.1, "semicircle should reach y=0, got {top}");
        assert_eq!(*out[0].0.last().unwrap(), (20.0, 10.0));
    }
}
