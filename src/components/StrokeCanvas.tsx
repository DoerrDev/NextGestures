import { useEffect, useRef } from 'react'
import type { Point, Region, TriggerButton, WheelDir } from '../api'
import arrowUpLeft from '../assets/icons/arrow-up-left.svg?raw'
import arrowUpRight from '../assets/icons/arrow-up-right.svg?raw'
import arrowDownLeft from '../assets/icons/arrow-down-left.svg?raw'
import arrowDownRight from '../assets/icons/arrow-down-right.svg?raw'
import gestureMouse from '../assets/icons/gesture-mouse.svg?raw'

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
const [MOUSE_BODY, MOUSE_LEFT, MOUSE_RIGHT, MOUSE_DIVIDER, MOUSE_WHEEL, MOUSE_SCROLL_UP, MOUSE_SCROLL_DOWN] = paths(gestureMouse)

interface Props {
  points: Point[]
  /** 与 points 对应的相对毫秒；有值时循环回放录制过程 */
  times?: number[]
  region?: Region | null
  wheel?: WheelDir[]
  trigger?: TriggerButton | null
  leftClick?: boolean
  /** 实时录制不循环播放，鼠标跟随当前采样终点。 */
  live?: boolean
  color?: string
  width?: number
  glowColor?: string
  glowBlur?: number
  className?: string
}

/** 回放结束后停留的时间 */
const HOLD_MS = 900
/** 每个滚轮方向独立演示半秒，再切换到下一个方向。 */
const WHEEL_STEP_MS = 500
const CLICK_PAUSE_MS = 180
const CLICK_PRESS_MS = 220
const CLICK_RELEASE_MS = 180
const MARK = '#5a5a5a'

/** 回放轨迹，并通过鼠标 SVG 展示按键、滚轮方向和终点点击。 */
export function StrokeCanvas({
  points,
  times,
  region,
  wheel = [],
  trigger,
  leftClick = false,
  live = false,
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
    /** 缩略图很小时按比例收窄留白，避免轨迹被压成一团 */
    const mouseSize = hasStroke
      ? Math.max(14, Math.min(30, Math.min(W, H) * 0.2))
      : Math.max(20, Math.min(64, Math.min(W, H) * 0.65))
    const showMouse = (hasStroke && !!trigger) || wheel.length > 0
    const pad = Math.max(showMouse ? mouseSize / 2 + 5 : 3,
      Math.min(14, Math.min(W, H) * 0.14))

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

    // 旧配置没有时间戳时，组合手势也能依次演示轨迹、滚轮和终点点击。
    const anim = !live && hasStroke
      ? times && times.length === points.length && times[times.length - 1] > 0
        ? times
        : leftClick || wheel.length > 0 ? points.map((_, i) => (i / (points.length - 1)) * 1200) : null
      : null
    const strokeTotal = anim ? anim[anim.length - 1] : 0
    const wheelTotal = live ? 0 : wheel.length * WHEEL_STEP_MS
    const clickStart = strokeTotal + wheelTotal + CLICK_PAUSE_MS
    const releaseAt = leftClick ? clickStart + CLICK_PRESS_MS + CLICK_RELEASE_MS : strokeTotal + wheelTotal

    const drawMouse = (x: number, y: number, t: number) => {
      if (!showMouse) return
      const wheelTime = live ? t : t - strokeTotal
      const wheelIndex = live ? wheel.length - 1 : Math.floor(wheelTime / WHEEL_STEP_MS)
      const scrolling = wheelTime >= 0 && wheelIndex >= 0 && wheelIndex < wheel.length
        && (!live || wheelTime < WHEEL_STEP_MS)
      const scrollDir = scrolling ? wheel[wheelIndex] : null
      const scrollProgress = (wheelTime % WHEEL_STEP_MS) / WHEEL_STEP_MS
      const rightDown = trigger === 'Right' && (live || t < releaseAt || (t === Infinity && leftClick))
      const leftDown = leftClick && (live || t === Infinity || (t >= clickStart && t < clickStart + CLICK_PRESS_MS))
      ctx.save()
      ctx.translate(x - mouseSize / 2, y - mouseSize / 2)
      ctx.scale(mouseSize / ICON, mouseSize / ICON)
      ctx.lineCap = 'round'
      ctx.lineJoin = 'round'
      ctx.fillStyle = '#fffdf7'
      ctx.fill(MOUSE_BODY)
      if (rightDown) {
        ctx.fillStyle = '#52b6f5'
        ctx.fill(MOUSE_RIGHT)
      }
      if (leftDown) {
        ctx.fillStyle = '#ffad56'
        ctx.fill(MOUSE_LEFT)
      }
      ctx.strokeStyle = '#383838'
      ctx.lineWidth = 1.5
      ctx.stroke(MOUSE_BODY)
      ctx.stroke(MOUSE_DIVIDER)
      ctx.fillStyle = scrolling ? '#d8efff' : '#fffdf7'
      ctx.fill(MOUSE_WHEEL)
      ctx.lineWidth = 1
      ctx.strokeStyle = scrolling || (trigger === 'Middle' && (live || t < releaseAt)) ? '#168dd1' : '#383838'
      ctx.stroke(MOUSE_WHEEL)
      if (scrollDir) {
        // 在滚轮内部移动刻纹，鼠标腹部的箭头同步向滚动方向轻移。
        const direction = scrollDir === 'Up' ? -1 : 1
        const offset = direction * scrollProgress * 6
        ctx.save()
        ctx.clip(MOUSE_WHEEL)
        ctx.lineWidth = 0.8
        for (let i = -3; i <= 4; i++) {
          const notchY = 4 + i * 2 + offset
          ctx.beginPath()
          ctx.moveTo(10.5, notchY)
          ctx.lineTo(13.5, notchY)
          ctx.stroke()
        }
        ctx.restore()
        ctx.save()
        ctx.translate(0, direction * Math.sin(scrollProgress * Math.PI) * 1.2)
        ctx.lineWidth = 1.6
        ctx.stroke(scrollDir === 'Up' ? MOUSE_SCROLL_UP : MOUSE_SCROLL_DOWN)
        ctx.restore()
      }
      ctx.restore()

      // 点击时扩散一圈，配合左键色块表达按下、抬起的瞬间。
      const elapsed = t - clickStart
      if (!live && leftClick && elapsed >= 0 && elapsed < CLICK_PRESS_MS + CLICK_RELEASE_MS) {
        const progress = elapsed / (CLICK_PRESS_MS + CLICK_RELEASE_MS)
        ctx.save()
        ctx.globalAlpha = 1 - progress
        ctx.strokeStyle = '#e68a2e'
        ctx.lineWidth = 1.5
        ctx.beginPath()
        ctx.arc(x - mouseSize * 0.15, y - mouseSize * 0.25, mouseSize * (0.2 + progress * 0.45), 0, Math.PI * 2)
        ctx.stroke()
        ctx.restore()
      }
    }

    let at = (p: Point): Point => p
    if (hasStroke) {
      const xs = points.map((p) => p[0])
      const ys = points.map((p) => p[1])
      const minX = Math.min(...xs)
      const minY = Math.min(...ys)
      const spanX = Math.max(...xs) - minX
      const spanY = Math.max(...ys) - minY
      const availH = H
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
        drawMouse(tx, ty, t)
      } else {
        drawMouse(W / 2, H / 2, t)
      }
    }

    const total = live ? (wheel.length > 0 ? WHEEL_STEP_MS : 0) : releaseAt
    if (total <= 0) {
      draw(Infinity)
      return
    }

    let raf = 0
    const start = performance.now()
    const cycle = total + HOLD_MS
    const tick = (now: number) => {
      const elapsed = now - start
      draw(live ? elapsed : elapsed % cycle)
      if (!live || elapsed < total) raf = requestAnimationFrame(tick)
    }
    raf = requestAnimationFrame(tick)
    return () => cancelAnimationFrame(raf)
  }, [points, times, region, wheel, trigger, leftClick, live, color, width, glowColor, glowBlur])

  return <canvas ref={ref} className={className} />
}
