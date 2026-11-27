use super::*;
use std::hint::black_box;
use std::time::Instant;

// Test-only prototype. In the engine, templates would be prepared on config changes
// and the input once per lookup. Keep the directional length penalty unchanged.
struct Prepared<'a> {
    points: &'a [Point],
    length: f32,
    stretched: Option<(Vec<Point>, f32)>,
}

impl<'a> Prepared<'a> {
    fn new(points: &'a [Point]) -> Self {
        Self {
            points,
            length: path_length(points),
            stretched: stretch_to_square(points).map(|points| {
                let length = path_length(&points);
                (points, length)
            }),
        }
    }
}

fn direct(a: &[Point], b: &[Point], la: f32, lb: f32) -> f32 {
    let mean = a.iter().zip(b).map(|(p, q)| dist(*p, *q)).sum::<f32>() / a.len() as f32;
    let base = (1.0 - mean / HALF_DIAGONAL).max(0.0);
    let factor = if la <= 1e-4 {
        1.0
    } else {
        let excess = (lb / la - LENGTH_TOLERANCE).max(0.0);
        1.0 / (1.0 + 1.5 * excess)
    };
    base * factor
}

fn prepared_score(a: &Prepared<'_>, b: &Prepared<'_>) -> f32 {
    if a.points.len() != b.points.len() || a.points.is_empty() {
        return 0.0;
    }
    let base = direct(a.points, b.points, a.length, b.length);
    match (&a.stretched, &b.stretched) {
        (Some((sa, la)), Some((sb, lb))) => base.max(direct(sa, sb, *la, *lb) * ASPECT_DISCOUNT),
        _ => base,
    }
}

fn template(index: usize) -> Vec<Point> {
    let width = 75.0 + (index % 9) as f32 * 15.0;
    let height = 65.0 + (index % 7) as f32 * 20.0;
    let raw = [
        [0.0, 0.0],
        [width, 0.0],
        [width, height],
        [width * 0.5, height],
    ];
    normalize(&raw).unwrap()
}

#[test]
fn prepared_scores_match_production_bit_for_bit() {
    let mut fixtures: Vec<_> = (0..64).map(template).collect();
    fixtures.extend([
        normalize(&[[0.0, 0.0], [100.0, 0.0]]).unwrap(),
        normalize(&[[100.0, 0.0], [0.0, 0.0]]).unwrap(),
        vec![[1.0, 1.0]; NUM_POINTS],
        vec![[0.0, 0.0], [10.0, 0.0], [10.0, 12.0]],
        vec![[10.0, 0.0], [0.0, 0.0], [0.0, 12.0]],
        vec![],
    ]);
    for a in &fixtures {
        for b in &fixtures {
            assert_eq!(
                score(a, b).to_bits(),
                prepared_score(&Prepared::new(a), &Prepared::new(b)).to_bits()
            );
        }
    }
}

#[test]
#[ignore = "timing baseline, run explicitly in release mode"]
fn perf_score_candidates() {
    const ITERATIONS: u32 = 200;
    for size in [1, 8, 32, 128] {
        let templates: Vec<_> = (0..size).map(template).collect();
        let prepared: Vec<_> = templates
            .iter()
            .map(|points| Prepared::new(points))
            .collect();
        let queries: Vec<_> = (0..16).map(|i| template(i * 7 + 2)).collect();
        let baseline = |query: &[Point]| {
            templates
                .iter()
                .map(|points| score(points, query))
                .sum::<f32>()
        };
        let prototype = |query: &[Point]| {
            let query = Prepared::new(query);
            prepared
                .iter()
                .map(|points| prepared_score(points, &query))
                .sum::<f32>()
        };
        for query in &queries {
            assert_eq!(baseline(query).to_bits(), prototype(query).to_bits());
        }
        let measure = |run: &dyn Fn(&[Point]) -> f32| {
            let start = Instant::now();
            for i in 0..ITERATIONS {
                black_box(run(black_box(&queries[i as usize % queries.len()])));
            }
            start.elapsed().as_secs_f64() * 1e6 / f64::from(ITERATIONS)
        };
        let mut before = Vec::new();
        let mut after = Vec::new();
        for round in 0..7 {
            if round % 2 == 0 {
                before.push(measure(&baseline));
                after.push(measure(&prototype));
            } else {
                after.push(measure(&prototype));
                before.push(measure(&baseline));
            }
        }
        before.sort_by(f64::total_cmp);
        after.sort_by(f64::total_cmp);
        println!(
            "{size:>3} curved candidates: {:.2} -> {:.2} us/match",
            before[3], after[3]
        );
    }
}
