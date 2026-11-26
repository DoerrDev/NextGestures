import { listen } from '@tauri-apps/api/event'
import { invoke } from '@tauri-apps/api/core'

interface TrailBegin {
  origin: [number, number]
  color: string
  width: number
  show_trail: boolean
  glow_color: string
  glow_blur: number
  miss_color: string
  hint_font_size: number
  hint_color: string
  hint_offset_x: number
  hint_offset_y: number
}

interface GestureHint {
  name: string
  font_size: number
  color: string
  offset_x: number
  offset_y: number
}

interface GestureHintPreview {
  font_size: number
  color: string
  offset_x: number
  offset_y: number
}

const canvas = document.getElementById('trail') as HTMLCanvasElement
const label = document.getElementById('label') as HTMLDivElement
const ctx = canvas.getContext('2d')!

let origin: [number, number] = [0, 0]
let style = {
  color: '#4d4dff',
  width: 3,
  showTrail: true,
  glowColor: '#00caf2',
  glowBlur: 8,
  missColor: '#a1a1a1',
}
let matched = false
let labelMode: 'preview' | 'toast' | 'settings' | null = null
let labelTimer = 0
let leaveTimer = 0
let labelOffset: [number, number] = [0, 0]
/** 每帧整条重画：用经过中点的二次贝塞尔平滑，避免逐段叠加带来的锯齿 */
const points: [number, number][] = []
let raf = 0
/**
 * 画布上是否有内容。实测只擦包围盒反而更费 GPU：整张 clearRect 能让 Chromium 直接丢弃旧帧，
 * 局部擦除则要保留并拷贝上一帧，所以仍整张清，只是空画布时跳过
 */
let dirty = false
let appliedFilter = ''
let trailEndTimer = 0
const registrations: Promise<unknown>[] = []
const replayHandlers = new Map<string, (payload: unknown) => void>()

function onOverlayEvent<T>(event: string, handle: (payload: T) => void) {
  replayHandlers.set(event, (payload) => handle(payload as T))
  registrations.push(listen<T>(event, ({ payload }) => handle(payload)))
}

label.addEventListener('animationend', (e) => {
  if (e.animationName !== 'hint-drop-in' || !labelMode) return
  label.classList.remove('visible')
  label.classList.add('static')
})

/** 赋值 canvas.width 即使数值不变也会重新分配整张全屏位图并重置上下文，只在尺寸真的变化时做 */
function resize() {
  const dpr = window.devicePixelRatio || 1
  const w = Math.round(window.innerWidth * dpr)
  const h = Math.round(window.innerHeight * dpr)
  if (canvas.width === w && canvas.height === h) return
  canvas.width = w
  canvas.height = h
  dirty = false
}

function clearDirty() {
  if (!dirty) return
  ctx.clearRect(0, 0, canvas.width, canvas.height)
  dirty = false
}

function clearTrail() {
  window.clearTimeout(trailEndTimer)
  clearDirty()
  matched = false
  points.length = 0
  cancelAnimationFrame(raf)
  raf = 0
}

function schedule() {
  if (!raf) {
    raf = requestAnimationFrame(() => {
      raf = 0
      repaint()
    })
  }
}

/** 画布只画锐利实线，辉光交给 CSS drop-shadow 由合成器出，不会逐段叠加也不吃 CPU */
function applyStyle() {
  const dpr = window.devicePixelRatio || 1
  ctx.lineCap = 'round'
  ctx.lineJoin = 'round'
  ctx.strokeStyle = matched ? style.color : style.missColor
  ctx.lineWidth = style.width * dpr
  // 每帧重设同样的 filter 也会触发样式重算
  const filter =
    matched && style.glowBlur > 0
      ? `drop-shadow(0 0 ${style.glowBlur}px ${style.glowColor})`
      : 'none'
  if (filter !== appliedFilter) {
    canvas.style.filter = filter
    appliedFilter = filter
  }
}

function repaint() {
  clearDirty()
  if (!style.showTrail) return
  if (points.length < 2) return
  resize()
  applyStyle()
  ctx.beginPath()
  ctx.moveTo(points[0][0], points[0][1])
  for (let i = 1; i < points.length - 1; i++) {
    const [x0, y0] = points[i]
    const [x1, y1] = points[i + 1]
    ctx.quadraticCurveTo(x0, y0, (x0 + x1) / 2, (y0 + y1) / 2)
  }
  const [ex, ey] = points[points.length - 1]
  ctx.lineTo(ex, ey)
  ctx.stroke()
  dirty = true
}

window.addEventListener('resize', () => {
  if (points.length) schedule()
  if (labelMode) placeLabel()
})

/** 传入的是虚拟屏幕物理坐标，减去窗口原点即为画布坐标 */
const toLocal = (p: [number, number]): [number, number] => [p[0] - origin[0], p[1] - origin[1]]

onOverlayEvent<TrailBegin>('trail:begin', (payload) => {
  clearTrail()
  if (labelMode !== 'settings') hideLabel(false)
  origin = payload.origin
  style = {
    color: payload.color,
    width: payload.width,
    showTrail: payload.show_trail,
    glowColor: payload.glow_color,
    glowBlur: payload.glow_blur,
    missColor: payload.miss_color,
  }
  if (labelMode !== 'settings') {
    setLabelStyle(
      payload.hint_font_size,
      payload.hint_color,
      payload.hint_offset_x,
      payload.hint_offset_y,
    )
  }
  applyStyle()
})

/** 后端按 ~4ms 批量推送（见 engine::TRAIL_FLUSH），关闭轨迹显示时不会推送 */
onOverlayEvent<[number, number][]>('trail:points', (payload) => {
  // 不显示轨迹时什么都不用画，也就不必每帧去碰全屏画布
  if (!style.showTrail) return
  for (const p of payload) points.push(toLocal(p))
  schedule()
})

onOverlayEvent<string | null>('trail:match', (name) => {
  if (labelMode === 'settings') return
  const hit = !!name
  if (hit !== matched) {
    matched = hit
    repaint()
  }
  if (!name) {
    if (labelMode === 'preview') hideLabel(true)
    return
  }
  showLabel(name, 'preview')
})

function endTrail() {
  clearTrail()
  if (labelMode === 'preview') hideLabel(true)
}
onOverlayEvent('trail:end', endTrail)

onOverlayEvent<GestureHint>('gesture:hint-show', (hint) => {
  setLabelStyle(hint.font_size, hint.color, hint.offset_x, hint.offset_y)
  showLabel(hint.name, 'preview')
})

onOverlayEvent('gesture:execute', () => {
  if (labelMode === 'preview') {
    labelTimer = window.setTimeout(() => {
      if (labelMode === 'preview') hideLabel(true)
    }, 100)
  }
})

onOverlayEvent<GestureHint>('gesture:toast', (hint) => {
  if (labelMode === 'settings') return
  setLabelStyle(hint.font_size, hint.color, hint.offset_x, hint.offset_y)
  showLabel(hint.name, 'toast')
  labelTimer = window.setTimeout(() => {
    if (labelMode === 'toast') hideLabel(true)
  }, 1400)
})

onOverlayEvent<GestureHintPreview>('gesture:hint-preview', (hint) => {
  setLabelStyle(hint.font_size, hint.color, hint.offset_x, hint.offset_y)
  if (labelMode !== 'settings') showLabel('打开 Jenkins', 'settings')
})

onOverlayEvent('gesture:hint-preview:hide', () => {
  if (labelMode === 'settings') hideLabel(true)
})

type PendingEvent = { event: string; payload: unknown }
registrations.push(listen<PendingEvent[]>('overlay:replay', ({ payload }) => {
  let lastTrailEvent = -1
  for (let i = payload.length - 1; i >= 0; i--) {
    if (payload[i].event === 'trail:begin' || payload[i].event === 'trail:end') {
      lastTrailEvent = i
      break
    }
  }
  payload.forEach(({ event, payload: value }, index) => {
    // If a quick gesture already ended during WebView startup, render its final stroke
    // briefly before clearing. A fresh trail cancels this delay.
    if (event === 'trail:end' && index === lastTrailEvent && points.length > 1) {
      schedule()
      trailEndTimer = window.setTimeout(endTrail, 120)
    } else {
      replayHandlers.get(event)?.(value)
    }
  })
}))
void Promise.all(registrations).then(() => {
  const generation = (window as Window & { __nextGesturesOverlayGeneration?: number }).__nextGesturesOverlayGeneration
  if (generation === undefined) return
  return invoke('overlay_ready', { generation })
}).catch((error) => console.error('Overlay initialization failed', error))

function setLabelStyle(fontSize: number, color: string, offsetX: number, offsetY: number) {
  label.style.fontSize = `${fontSize}px`
  label.style.color = color
  labelOffset = [offsetX, offsetY]
}

function placeLabel() {
  const x = Math.round((window.innerWidth - label.offsetWidth) / 2 + labelOffset[0])
  const y = Math.round((window.innerHeight - label.offsetHeight) / 2 + labelOffset[1])
  label.style.left = `${x}px`
  label.style.top = `${y}px`
}

function showLabel(text: string, mode: 'preview' | 'toast' | 'settings') {
  window.clearTimeout(labelTimer)
  window.clearTimeout(leaveTimer)
  labelMode = mode
  label.textContent = text
  label.classList.remove('visible', 'leaving', 'static')
  placeLabel()
  if (mode === 'settings') {
    label.classList.add('static')
    return
  }
  void label.offsetWidth
  label.classList.add('visible')
}

function hideLabel(animated: boolean) {
  window.clearTimeout(labelTimer)
  window.clearTimeout(leaveTimer)
  labelMode = null
  label.classList.remove('visible', 'leaving', 'static')
  if (!animated) return
  label.classList.add('leaving')
  leaveTimer = window.setTimeout(() => label.classList.remove('leaving'), 320)
}
