import { listen } from '@tauri-apps/api/event'

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

label.addEventListener('animationend', (e) => {
  if (e.animationName !== 'hint-drop-in' || !labelMode) return
  label.classList.remove('visible')
  label.classList.add('static')
})

function resize() {
  const dpr = window.devicePixelRatio || 1
  canvas.width = Math.round(window.innerWidth * dpr)
  canvas.height = Math.round(window.innerHeight * dpr)
}

function clearTrail() {
  ctx.clearRect(0, 0, canvas.width, canvas.height)
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
  canvas.style.filter =
    matched && style.glowBlur > 0
      ? `drop-shadow(0 0 ${style.glowBlur}px ${style.glowColor})`
      : 'none'
}

function repaint() {
  ctx.clearRect(0, 0, canvas.width, canvas.height)
  if (!style.showTrail) return
  applyStyle()
  if (points.length < 2) return
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
}

resize()
window.addEventListener('resize', () => {
  resize()
  clearTrail()
  if (labelMode !== 'settings') hideLabel(false)
  else placeLabel()
  applyStyle()
})

/** 传入的是虚拟屏幕物理坐标，减去窗口原点即为画布坐标 */
const toLocal = (p: [number, number]): [number, number] => [p[0] - origin[0], p[1] - origin[1]]

void listen<TrailBegin>('trail:begin', (e) => {
  resize()
  clearTrail()
  if (labelMode !== 'settings') hideLabel(false)
  origin = e.payload.origin
  style = {
    color: e.payload.color,
    width: e.payload.width,
    showTrail: e.payload.show_trail,
    glowColor: e.payload.glow_color,
    glowBlur: e.payload.glow_blur,
    missColor: e.payload.miss_color,
  }
  if (labelMode !== 'settings') {
    setLabelStyle(
      e.payload.hint_font_size,
      e.payload.hint_color,
      e.payload.hint_offset_x,
      e.payload.hint_offset_y,
    )
  }
  applyStyle()
})

void listen<[number, number]>('trail:point', (e) => {
  const p = toLocal(e.payload)
  points.push(p)
  schedule()
})

void listen<string | null>('trail:match', (e) => {
  if (labelMode === 'settings') return
  const hit = !!e.payload
  if (hit !== matched) {
    matched = hit
    repaint()
  }
  if (!e.payload) {
    if (labelMode === 'preview') hideLabel(true)
    return
  }
  showLabel(e.payload, 'preview')
})

void listen('trail:end', () => {
  clearTrail()
  if (labelMode === 'preview') hideLabel(true)
})

void listen<GestureHint>('gesture:hint-show', (e) => {
  setLabelStyle(e.payload.font_size, e.payload.color, e.payload.offset_x, e.payload.offset_y)
  showLabel(e.payload.name, 'preview')
})

void listen('gesture:execute', () => {
  if (labelMode === 'preview') window.setTimeout(() => hideLabel(true), 100)
})

void listen<GestureHint>('gesture:toast', (e) => {
  if (labelMode === 'settings') return
  setLabelStyle(e.payload.font_size, e.payload.color, e.payload.offset_x, e.payload.offset_y)
  showLabel(e.payload.name, 'toast')
  labelTimer = window.setTimeout(() => {
    if (labelMode === 'toast') hideLabel(true)
  }, 1400)
})

void listen<GestureHintPreview>('gesture:hint-preview', (e) => {
  setLabelStyle(e.payload.font_size, e.payload.color, e.payload.offset_x, e.payload.offset_y)
  if (labelMode !== 'settings') showLabel('打开 Jenkins', 'settings')
})

void listen('gesture:hint-preview:hide', () => {
  if (labelMode === 'settings') hideLabel(true)
})

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
