import type { Gesture, Point } from './api'

const HALF_DIAGONAL = 176.7767
const LENGTH_TOLERANCE = 1.25
const SQUARE_SIZE = 250
const ASPECT_MIN = 0.2
const ASPECT_DISCOUNT = 0.97

function dist(a: Point, b: Point): number {
  return Math.hypot(a[0] - b[0], a[1] - b[1])
}

function pathLength(pts: Point[]): number {
  let sum = 0
  for (let i = 1; i < pts.length; i++) sum += dist(pts[i - 1], pts[i])
  return sum
}

function resample(pts: Point[], n: number): Point[] {
  const interval = pathLength(pts) / (n - 1)
  const out: Point[] = [pts[0]]
  let prev = pts[0]
  let acc = 0
  let i = 1
  while (i < pts.length && out.length < n) {
    const cur = pts[i]
    const seg = dist(prev, cur)
    if (acc + seg >= interval && seg > 0) {
      const t = Math.min(1, Math.max(0, (interval - acc) / seg))
      const np: Point = [prev[0] + t * (cur[0] - prev[0]), prev[1] + t * (cur[1] - prev[1])]
      out.push(np)
      prev = np
      acc = 0
    } else {
      acc += seg
      prev = cur
      i++
    }
  }
  while (out.length < n) out.push(pts[pts.length - 1])
  return out
}

/** bbox 非等比拉满成正方形；细长轨迹返回 null */
function stretchToSquare(pts: Point[]): Point[] | null {
  let minx = Infinity
  let miny = Infinity
  let maxx = -Infinity
  let maxy = -Infinity
  for (const p of pts) {
    minx = Math.min(minx, p[0])
    miny = Math.min(miny, p[1])
    maxx = Math.max(maxx, p[0])
    maxy = Math.max(maxy, p[1])
  }
  const w = maxx - minx
  const h = maxy - miny
  const hi = Math.max(w, h)
  if (hi <= 1e-4 || Math.min(w, h) / hi < ASPECT_MIN) return null

  const stretched: Point[] = pts.map((p) => [
    ((p[0] - minx) * SQUARE_SIZE) / w,
    ((p[1] - miny) * SQUARE_SIZE) / h,
  ])
  const de: Point[] = []
  for (const p of stretched) {
    if (!de.length || dist(de[de.length - 1], p) > 1e-4) de.push(p)
  }
  if (de.length < 2 || pathLength(de) <= 1e-4) return null

  const out = resample(de, pts.length)
  const cx = out.reduce((s, p) => s + p[0], 0) / out.length
  const cy = out.reduce((s, p) => s + p[1], 0) / out.length
  return out.map((p) => [p[0] - cx, p[1] - cy])
}

function scoreDirect(a: Point[], b: Point[]): number {
  let sum = 0
  for (let i = 0; i < a.length; i++) sum += dist(a[i], b[i])
  const base = Math.max(0, 1 - sum / a.length / HALF_DIAGONAL)
  const la = pathLength(a)
  if (la <= 1e-4) return base
  const excess = Math.max(0, pathLength(b) / la - LENGTH_TOLERANCE)
  return base / (1 + 1.5 * excess)
}

/** 与 crates/core/src/recognizer.rs 的 score 保持一致 */
function score(a: Point[], b: Point[]): number {
  if (a.length !== b.length || a.length === 0) return 0
  const base = scoreDirect(a, b)
  const sa = stretchToSquare(a)
  const sb = stretchToSquare(b)
  if (!sa || !sb) return base
  return Math.max(base, scoreDirect(sa, sb) * ASPECT_DISCOUNT)
}

export interface ConflictPair {
  a: Gesture
  b: Gesture
}

export interface ConflictReport {
  pairs: ConflictPair[]
  ids: Set<string>
}

function sameTriggerContext(a: Gesture, b: Gesture): boolean {
  return (
    a.trigger === b.trigger &&
    a.region === b.region &&
    a.wheel.length === b.wheel.length &&
    a.wheel.every((w, i) => w === b.wheel[i])
  )
}

/** 找出触发条件相同、轨迹又相似到会互相抢匹配的手势对 */
export function findConflicts(gestures: Gesture[], threshold: number): ConflictReport {
  const list = gestures.filter((g) => g.enabled)
  const pairs: ConflictPair[] = []
  const ids = new Set<string>()

  for (let i = 0; i < list.length; i++) {
    for (let j = i + 1; j < list.length; j++) {
      const a = list[i]
      const b = list[j]
      if (!sameTriggerContext(a, b)) continue

      const empty = a.points.length === 0 && b.points.length === 0
      const s = empty ? 1 : Math.max(score(a.points, b.points), score(b.points, a.points))
      if (s < threshold) continue

      pairs.push({ a, b })
      ids.add(a.id)
      ids.add(b.id)
    }
  }
  return { pairs, ids }
}
