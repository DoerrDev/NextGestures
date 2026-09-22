pub type Point = [f32; 2];

pub const NUM_POINTS: usize = 64;
pub const SQUARE_SIZE: f32 = 250.0;

const HALF_DIAGONAL: f32 = 176.776_7; // 0.5 * sqrt(2) * SQUARE_SIZE

/// 把原始采样点归一化成可比较的模板：重采样 -> 等比缩放 -> 中心平移。
/// 故意不做 $1 的旋转归一化，否则 "↓" 和 "→" 会互相匹配。
pub fn normalize(raw: &[Point]) -> Option<Vec<Point>> {
    let pts = dedup(raw);
    if pts.len() < 2 || path_length(&pts) <= 1e-4 {
        return None;
    }
    let mut out = resample(&pts, NUM_POINTS);
    scale_uniform(&mut out);
    translate_to_origin(&mut out);
    for p in &mut out {
        p[0] = (p[0] * 100.0).round() / 100.0;
        p[1] = (p[1] * 100.0).round() / 100.0;
    }
    Some(out)
}

/// 把原始采样时间戳映射到 NUM_POINTS 个重采样点上（相对起点的毫秒数）。
/// 重采样点按路径长度等距分布，因此第 i 点对应弧长比例 i/(n-1)。
pub fn timeline(raw: &[Point], times: &[u32]) -> Vec<f32> {
    if raw.len() != times.len() || raw.len() < 2 {
        return Vec::new();
    }
    let mut cum = Vec::with_capacity(raw.len());
    cum.push(0.0f32);
    for w in raw.windows(2) {
        cum.push(cum.last().unwrap() + dist(w[0], w[1]));
    }
    let total = *cum.last().unwrap();
    if total <= 1e-4 {
        return Vec::new();
    }
    let t0 = times[0];
    let rel = |i: usize| times[i].wrapping_sub(t0) as f32;
    let mut seg = 1;
    (0..NUM_POINTS)
        .map(|i| {
            let target = total * i as f32 / (NUM_POINTS - 1) as f32;
            while seg < cum.len() - 1 && cum[seg] < target {
                seg += 1;
            }
            let (a, b) = (cum[seg - 1], cum[seg]);
            let f = if b > a { ((target - a) / (b - a)).clamp(0.0, 1.0) } else { 1.0 };
            (rel(seg - 1) + f * (rel(seg) - rel(seg - 1))).round()
        })
        .collect()
}

/// 允许的路径长度冗余：手抖、多画一点点不算
const LENGTH_TOLERANCE: f32 = 1.25;

/// 走拉伸分支所需的最小「短边/长边」：低于此值视为直线类轨迹。
/// 直线的 bbox 有一边接近 0，拉伸会把手抖放大成形状，而且横线和竖线
/// 拉成正方形后是同一条对角线，必须保持等比。
const ASPECT_MIN: f32 = 0.2;

/// 拉伸分支的轻微折扣，保证连比例都吻合的手势仍以等比结果取胜
const ASPECT_DISCOUNT: f32 = 0.97;

/// 两个已归一化模板的相似度，[0,1]，越大越像。
/// `a` 是模板，`b` 是待识别轨迹。除了逐点距离，还按归一化后的路径长度惩罚
/// 「画得太多」——比如在 L 之后又乱涂一通，逐点距离仍可能不低，但路径长得多。
///
/// 等比缩放会把「同一个形状画得扁还是瘦高」也算进距离里，所以两边都不是
/// 直线类轨迹时，再各自把 bbox 拉满成正方形比一次，取较高分。
pub fn score(a: &[Point], b: &[Point]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let base = score_direct(a, b);
    match (stretch_to_square(a), stretch_to_square(b)) {
        (Some(sa), Some(sb)) => base.max(score_direct(&sa, &sb) * ASPECT_DISCOUNT),
        _ => base,
    }
}

fn score_direct(a: &[Point], b: &[Point]) -> f32 {
    let mean = a
        .iter()
        .zip(b)
        .map(|(p, q)| dist(*p, *q))
        .sum::<f32>()
        / a.len() as f32;
    let base = (1.0 - mean / HALF_DIAGONAL).max(0.0);
    base * length_factor(a, b)
}

/// 把 bbox 非等比拉满到 SQUARE_SIZE 的正方形，消掉宽高比这个自由度。
/// 细长轨迹返回 None，交回等比分支处理。
fn stretch_to_square(pts: &[Point]) -> Option<Vec<Point>> {
    let (minx, miny, maxx, maxy) = bounds(pts);
    let (w, h) = (maxx - minx, maxy - miny);
    let (lo, hi) = (w.min(h), w.max(h));
    if hi <= 1e-4 || lo / hi < ASPECT_MIN {
        return None;
    }
    let stretched: Vec<Point> = pts
        .iter()
        .map(|p| [(p[0] - minx) * SQUARE_SIZE / w, (p[1] - miny) * SQUARE_SIZE / h])
        .collect();
    // 拉伸改变了各段的相对长度，必须按新弧长重采样，否则两条轨迹的点对不上
    let de = dedup(&stretched);
    if de.len() < 2 || path_length(&de) <= 1e-4 {
        return None;
    }
    let mut out = resample(&de, NUM_POINTS);
    translate_to_origin(&mut out);
    Some(out)
}

fn length_factor(a: &[Point], b: &[Point]) -> f32 {
    let la = path_length(a);
    let lb = path_length(b);
    if la <= 1e-4 {
        return 1.0;
    }
    let excess = (lb / la - LENGTH_TOLERANCE).max(0.0);
    1.0 / (1.0 + 1.5 * excess)
}

fn dedup(raw: &[Point]) -> Vec<Point> {
    let mut out: Vec<Point> = Vec::with_capacity(raw.len());
    for p in raw {
        if out.last().is_none_or(|l| dist(*l, *p) > 1e-4) {
            out.push(*p);
        }
    }
    out
}

fn dist(a: Point, b: Point) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
}

fn path_length(pts: &[Point]) -> f32 {
    pts.windows(2).map(|w| dist(w[0], w[1])).sum()
}

fn resample(pts: &[Point], n: usize) -> Vec<Point> {
    let interval = path_length(pts) / (n - 1) as f32;
    let mut out = vec![pts[0]];
    let mut prev = pts[0];
    let mut acc = 0.0f32;
    let mut i = 1;
    while i < pts.len() && out.len() < n {
        let cur = pts[i];
        let seg = dist(prev, cur);
        if acc + seg >= interval && seg > 0.0 {
            let t = ((interval - acc) / seg).clamp(0.0, 1.0);
            let np = [prev[0] + t * (cur[0] - prev[0]), prev[1] + t * (cur[1] - prev[1])];
            out.push(np);
            prev = np;
            acc = 0.0;
        } else {
            acc += seg;
            prev = cur;
            i += 1;
        }
    }
    let last = *pts.last().unwrap();
    while out.len() < n {
        out.push(last);
    }
    out
}

fn bounds(pts: &[Point]) -> (f32, f32, f32, f32) {
    let (mut minx, mut miny) = (f32::MAX, f32::MAX);
    let (mut maxx, mut maxy) = (f32::MIN, f32::MIN);
    for p in pts {
        minx = minx.min(p[0]);
        miny = miny.min(p[1]);
        maxx = maxx.max(p[0]);
        maxy = maxy.max(p[1]);
    }
    (minx, miny, maxx, maxy)
}

fn scale_uniform(pts: &mut [Point]) {
    let (minx, miny, maxx, maxy) = bounds(pts);
    let m = (maxx - minx).max(maxy - miny);
    if m <= 1e-4 {
        return;
    }
    let s = SQUARE_SIZE / m;
    for p in pts.iter_mut() {
        p[0] *= s;
        p[1] *= s;
    }
}

fn translate_to_origin(pts: &mut [Point]) {
    let n = pts.len() as f32;
    let cx = pts.iter().map(|p| p[0]).sum::<f32>() / n;
    let cy = pts.iter().map(|p| p[1]).sum::<f32>() / n;
    for p in pts.iter_mut() {
        p[0] -= cx;
        p[1] -= cy;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(from: Point, to: Point, n: usize) -> Vec<Point> {
        (0..n)
            .map(|i| {
                let t = i as f32 / (n - 1) as f32;
                [from[0] + t * (to[0] - from[0]), from[1] + t * (to[1] - from[1])]
            })
            .collect()
    }

    fn circle(cx: f32, cy: f32, r: f32, n: usize) -> Vec<Point> {
        (0..n)
            .map(|i| {
                let a = i as f32 / (n - 1) as f32 * std::f32::consts::TAU;
                [cx + r * a.cos(), cy + r * a.sin()]
            })
            .collect()
    }

    #[test]
    fn scale_and_translate_invariant() {
        let a = normalize(&line([0.0, 0.0], [100.0, 0.0], 20)).unwrap();
        let b = normalize(&line([500.0, 300.0], [1300.0, 300.0], 7)).unwrap();
        assert!(score(&a, &b) > 0.99, "{}", score(&a, &b));
    }

    #[test]
    fn rotation_sensitive() {
        let right = normalize(&line([0.0, 0.0], [100.0, 0.0], 20)).unwrap();
        let down = normalize(&line([0.0, 0.0], [0.0, 100.0], 20)).unwrap();
        assert!(score(&right, &down) < 0.5, "{}", score(&right, &down));
    }

    #[test]
    fn circle_matches_circle() {
        let a = normalize(&circle(0.0, 0.0, 50.0, 40)).unwrap();
        let b = normalize(&circle(800.0, 600.0, 130.0, 25)).unwrap();
        assert!(score(&a, &b) > 0.95, "{}", score(&a, &b));
        let l = normalize(&line([0.0, 0.0], [100.0, 100.0], 20)).unwrap();
        assert!(score(&a, &l) < 0.8, "{}", score(&a, &l));
    }

    #[test]
    fn extra_scribble_breaks_the_match() {
        let mut l = line([0.0, 0.0], [0.0, 200.0], 20);
        l.extend(line([0.0, 200.0], [200.0, 200.0], 20));
        let template = normalize(&l).unwrap();
        assert!(score(&template, &normalize(&l).unwrap()) > 0.99);

        let mut messy = l.clone();
        messy.extend(circle(100.0, 100.0, 150.0, 60));
        let messy = normalize(&messy).unwrap();
        assert!(score(&template, &messy) < 0.6, "{}", score(&template, &messy));
    }

    fn stretch(pts: &[Point], sx: f32, sy: f32) -> Vec<Point> {
        pts.iter().map(|p| [p[0] * sx, p[1] * sy]).collect()
    }

    /// "3"：上下两段弧，宽高比接近 1
    fn three() -> Vec<Point> {
        let mut p = Vec::new();
        for i in 0..40 {
            let t = i as f32 / 39.0 * std::f32::consts::PI * 1.4 - 0.5;
            p.push([108.0 + 108.0 * t.cos(), 60.0 + 55.0 * t.sin()]);
        }
        for i in 0..40 {
            let t = i as f32 / 39.0 * std::f32::consts::PI * 1.4 - 0.9;
            p.push([108.0 + 117.0 * t.cos(), 165.0 + 60.0 * t.sin()]);
        }
        p
    }

    #[test]
    fn aspect_insensitive_for_shapes() {
        let t = normalize(&three()).unwrap();
        for (sx, sy) in [(1.0, 2.0), (1.0, 3.0), (1.0, 4.0), (2.0, 1.0), (3.0, 1.0)] {
            let drawn = normalize(&stretch(&three(), sx, sy)).unwrap();
            let s = score(&t, &drawn);
            assert!(s > 0.9, "{sx}x{sy} -> {s}");
        }
        let c = normalize(&circle(0.0, 0.0, 50.0, 40)).unwrap();
        let oval = normalize(&stretch(&circle(0.0, 0.0, 50.0, 40), 1.0, 3.0)).unwrap();
        assert!(score(&c, &oval) > 0.9, "{}", score(&c, &oval));
    }

    /// 细长轨迹不走拉伸分支，否则横线和竖线拉成正方形后会变成同一条对角线
    #[test]
    fn thin_strokes_keep_aspect() {
        let right = normalize(&line([0.0, 0.0], [200.0, 0.0], 20)).unwrap();
        let down = normalize(&line([0.0, 0.0], [0.0, 200.0], 20)).unwrap();
        let diag = normalize(&line([0.0, 0.0], [140.0, 200.0], 20)).unwrap();
        assert!(score(&right, &down) < 0.5, "{}", score(&right, &down));
        assert!(score(&right, &diag) < 0.8, "{}", score(&right, &diag));
        // 形状模板遇到一条斜线也不该被拉伸救活
        let t = normalize(&three()).unwrap();
        assert!(score(&t, &diag) < 0.8, "{}", score(&t, &diag));
    }

    #[test]
    fn degenerate_returns_none() {
        assert!(normalize(&[]).is_none());
        assert!(normalize(&[[1.0, 1.0]]).is_none());
        assert!(normalize(&[[1.0, 1.0], [1.0, 1.0]]).is_none());
    }

    #[test]
    fn always_num_points() {
        let s = normalize(&line([0.0, 0.0], [3.0, 0.0], 2)).unwrap();
        assert_eq!(s.len(), NUM_POINTS);
    }
}
