import { useEffect, useRef } from 'react'
import type { Point, Region, WheelDir } from '../api'
import arrowUpLeft from '../assets/icons/arrow-up-left.svg?raw'
import arrowUpRight from '../assets/icons/arrow-up-right.svg?raw'
import arrowDownLeft from '../assets/icons/arrow-down-left.svg?raw'
import arrowDownRight from '../assets/icons/arrow-down-right.svg?raw'
import triangle from '../assets/icons/triangle.svg?raw'

/** 取出 lucide svg 里的所有 path（24x24 视口） */
function paths(svg: string): Path2D[] {
  return [...svg.matchAll(/ d="([^"]+)"/g)].map((m) => new Path2D(m[1]))
}

const ICON = 24
const CORNER_ICON: Partial<Record<Region, Path2D[]>> = {
  TopLeft: paths(arrowUpLeft),
  TopRight: paths(arrowUpRight),
  BottomLeft: paths(arrowDownLeft),
  BottomRight: paths(arrowDownRight),
}
const TRIANGLE = paths(triangle)

interface Props {
  points: Point[]
  /** 与 points 对应的相对毫秒；有值时循环回放录制过程 */
  times?: number[]
  region?: Region | null
  wheel?: WheelDir[]
  color?: string
  width?: number
  glowColor?: string
  glowBlur?: number
  className?: string
}

/** 回放结束后停留的时间 */
const HOLD_MS = 900
/** 回放中每个滚轮箭头出现的间隔 */
const WHEEL_STEP_MS = 250
const MARK = '#5a5a5a'

/** 把任意坐标系的轨迹等比铺进画布，并标出起点与终点方向；同时画出边角标记与滚轮序列 */
export function StrokeCanvas({
  points,
  times,
  region,
  wheel = [],
  color = '#383838',
  width = 3,
  glowColor,
  glowBlur = 0,
  className,
}: Props) {
  const ref = useRef<HTMLCanvasElement>(null)

  useEffect(() => {
    const canvas = ref.current
    if (!canvas) return
    const box = canvas.getBoundingClientRect()
    /** 小缩略图额外超采样，否则细线在低 dpr 下发糊 */
    const dpr = (window.devicePixelRatio || 1) * (box.width < 160 ? 2 : 1)
    canvas.width = Math.max(1, Math.round(box.width * dpr))
    canvas.height = Math.max(1, Math.round(box.height * dpr))
    const ctx = canvas.getContext('2d')
    if (!ctx) return
    ctx.scale(dpr, dpr)
    ctx.clearRect(0, 0, box.width, box.height)

    const W = box.width
    const H = box.height
    const hasStroke = points.length >= 2
    /** 滚轮箭头占用底部一条 */
    const band = wheel.length > 0 ? Math.min(28, H * 0.22) : 0
    /** 缩略图很小时按比例收窄留白，避免轨迹被压成一团 */
    const pad = Math.max(3, Math.min(14, Math.min(W, H - band) * 0.14))

    const drawRegion = () => {
      if (!region) return
      ctx.save()
      ctx.strokeStyle = MARK
      ctx.lineCap = 'round'
      ctx.lineJoin = 'round'
      const icon = CORNER_ICON[region]
      if (icon) {
        const size = 14
        const inset = 6
        const x = region.endsWith('Left') ? inset : W - inset - size
        const y = region.startsWith('Top') ? inset : H - inset - size
        ctx.translate(x, y)
        ctx.scale(size / ICON, size / ICON)
        ctx.lineWidth = 2
        icon.forEach((p) => ctx.stroke(p))
      } else {
        ctx.lineWidth = 3
        ctx.beginPath()
        switch (region) {
          case 'Top': ctx.moveTo(0, 1.5); ctx.lineTo(W, 1.5); break
          case 'Bottom': ctx.moveTo(0, H - 1.5); ctx.lineTo(W, H - 1.5); break
          case 'Left': ctx.moveTo(1.5, 0); ctx.lineTo(1.5, H); break
          case 'Right': ctx.moveTo(W - 1.5, 0); ctx.lineTo(W - 1.5, H); break
        }
        ctx.stroke()
      }
      ctx.restore()
    }

    /** 画前 n 个滚轮箭头 */
    const drawWheel = (n: number) => {
      if (wheel.length === 0) return
      const size = Math.min(band * 0.7, 18) || 18
      const gap = size * 1.5
      const cy = hasStroke ? H - band / 2 : H / 2
      const x0 = W / 2 - ((wheel.length - 1) * gap) / 2
      ctx.save()
      ctx.fillStyle = color
      for (let i = 0; i < Math.min(n, wheel.length); i++) {
        ctx.save()
        ctx.translate(x0 + i * gap, cy)
        if (wheel[i] === 'Down') ctx.rotate(Math.PI)
        ctx.scale(size / ICON, size / ICON)
        ctx.translate(-ICON / 2, -ICON / 2)
        TRIANGLE.forEach((p) => ctx.fill(p))
        ctx.restore()
      }
      ctx.restore()
    }

    const anim = hasStroke && times && times.length === points.length ? times : null
    const strokeTotal = anim ? anim[anim.length - 1] : 0
    const wheelTotal = wheel.length * WHEEL_STEP_MS

    let at = (p: Point): Point => p
    if (hasStroke) {
      const xs = points.map((p) => p[0])
      const ys = points.map((p) => p[1])
      const minX = Math.min(...xs)
      const minY = Math.min(...ys)
      const spanX = Math.max(...xs) - minX
      const spanY = Math.max(...ys) - minY
      const availH = H - band
      const scale = Math.min(
        (W - pad * 2) / Math.max(spanX, 1e-3),
        (availH - pad * 2) / Math.max(spanY, 1e-3),
      )
      const offX = (W - spanX * scale) / 2
      const offY = (availH - spanY * scale) / 2
      at = (p) => [(p[0] - minX) * scale + offX, (p[1] - minY) * scale + offY]
    }

    /** 画出到时刻 t 为止的画面；t 为 Infinity 时画完整 */
    const draw = (t: number) => {
      ctx.clearRect(0, 0, W, H)
      drawRegion()

      if (hasStroke) {
        let end = points.length - 1
        let tip: Point = points[end]
        if (anim && t < anim[end]) {
          end = anim.findIndex((v) => v > t) - 1
          if (end < 0) end = 0
          const a = points[end]
          const b = points[end + 1]
          const span = anim[end + 1] - anim[end]
          const f = span > 0 ? (t - anim[end]) / span : 1
          tip = [a[0] + (b[0] - a[0]) * f, a[1] + (b[1] - a[1]) * f]
        }

        ctx.lineWidth = width
        ctx.lineCap = 'round'
        ctx.lineJoin = 'round'
        ctx.strokeStyle = color
        if (glowColor && glowBlur > 0) {
          ctx.shadowColor = glowColor
          ctx.shadowBlur = glowBlur
        }
        ctx.beginPath()
        for (let i = 0; i <= end; i++) {
          const [x, y] = at(points[i])
          i === 0 ? ctx.moveTo(x, y) : ctx.lineTo(x, y)
        }
        const [tx, ty] = at(tip)
        ctx.lineTo(tx, ty)
        ctx.stroke()

        ctx.shadowBlur = 0

        const [sx, sy] = at(points[0])
        ctx.fillStyle = color
        ctx.beginPath()
        ctx.arc(sx, sy, width + 1.5, 0, Math.PI * 2)
        ctx.fill()

        if (end === points.length - 1) {
          ctx.strokeStyle = '#383838'
          ctx.lineWidth = 1
          ctx.beginPath()
          ctx.arc(tx, ty, width + 2.5, 0, Math.PI * 2)
          ctx.stroke()
        }
      }

      const wt = t - strokeTotal
      drawWheel(t === Infinity ? wheel.length : Math.floor(wt / WHEEL_STEP_MS) + 1)
    }

    const total = strokeTotal + wheelTotal
    if (total <= 0) {
      draw(Infinity)
      return
    }

    let raf = 0
    const start = performance.now()
    const cycle = total + HOLD_MS
    const tick = (now: number) => {
      draw((now - start) % cycle)
      raf = requestAnimationFrame(tick)
    }
    raf = requestAnimationFrame(tick)
    return () => cancelAnimationFrame(raf)
  }, [points, times, region, wheel, color, width, glowColor, glowBlur])

  return <canvas ref={ref} className={className} />
}
