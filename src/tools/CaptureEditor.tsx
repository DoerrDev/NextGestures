import { useEffect, useRef, useState, type PointerEvent } from 'react'
import { ArrowUpRight, Check, Copy, Grid3X3, MousePointer2, Pencil, PictureInPicture2, Pin, Redo2, Save, ScanText, Square, Text, Undo2, X } from 'lucide-react'
import { screens, TOOL_NAMES, type OcrLanguage, type Rect, type ScreenSession } from './api'
import { clampPoint, cropped, physicalPoint, rectangle, type Mark, type MarkKind, type XY } from './canvas'

type Mode = 'select' | MarkKind
type Drag = { kind: 'select' | 'move' | 'resize' | 'draw'; start: XY; base?: Rect; handle?: string; mark?: Mark }
const MODES = [
  ['select', '调整选区', MousePointer2], ['rectangle', '矩形', Square], ['arrow', '箭头', ArrowUpRight],
  ['pen', '画笔', Pencil], ['text', '文字', Text], ['mosaic', '马赛克', Grid3X3],
] as const

export function CaptureEditor({ session }: { session: ScreenSession }) {
  const [image, setImage] = useState<HTMLImageElement | null>(null)
  const [selection, setSelection] = useState<Rect | null>(null)
  const [mode, setMode] = useState<Mode>('select')
  const [marks, setMarks] = useState<Mark[]>([])
  const [redo, setRedo] = useState<Mark[]>([])
  const [pending, setPending] = useState<Mark | null>(null)
  const [interacting, setInteracting] = useState(false)
  const [color, setColor] = useState('#f34b43')
  const [width, setWidth] = useState(4)
  const [busy, setBusy] = useState('')
  const busyRef = useRef(false)
  const [error, setError] = useState('')
  const [message, setMessage] = useState('')
  const [text, setText] = useState<{ point: XY; value: string } | null>(null)
  const [ocrOpen, setOcrOpen] = useState(false)
  const [ocrText, setOcrText] = useState('')
  const [languages, setLanguages] = useState<OcrLanguage[]>([])
  const [language, setLanguage] = useState('')
  const background = useRef<HTMLCanvasElement>(null)
  const preview = useRef<HTMLCanvasElement>(null)
  const drag = useRef<Drag | null>(null)
  const bounds = session.bounds

  useEffect(() => {
    let alive = true
    const img = new Image(); img.crossOrigin = 'anonymous'; img.src = screens.image(session.label)
    void img.decode().then(() => { if (alive) setImage(img) }).catch(e => { if (alive) setError(`画面加载失败：${String(e)}`) })
    return () => { alive = false }
  }, [session.label])
  useEffect(() => { if (image && background.current) background.current.getContext('2d')!.drawImage(image, 0, 0) }, [image])
  useEffect(() => {
    if (!image || !selection || !preview.current || !selection.width || !selection.height) return
    const target = preview.current, frame = cropped(image, selection, pending ? [...marks, pending] : marks)
    target.getContext('2d')!.clearRect(0, 0, target.width, target.height)
    target.getContext('2d')!.drawImage(frame, 0, 0)
  }, [image, selection, marks, pending])

  const close = () => void screens.control(session.label, 'close').catch(e => setError(String(e)))
  const undo = () => {
    if (!marks.length) return
    setRedo([...redo, marks[marks.length - 1]]); setMarks(marks.slice(0, -1))
  }
  const redoMark = () => {
    if (!redo.length) return
    setMarks([...marks, redo[redo.length - 1]]); setRedo(redo.slice(0, -1))
  }
  const output = async (kind: string, region = selection) => {
    if (!image || !region || region.width < 2 || region.height < 2 || busyRef.current) return
    busyRef.current = true; setBusy(kind === 'ocr' ? '正在识别文字…' : kind === 'pip' ? '正在打开画中画…' : '正在处理图片…'); setError(''); setMessage('')
    try {
      if (kind === 'ocr') {
        setOcrOpen(true)
        const available = await screens.languages(); setLanguages(available)
        if (!available.length) throw Error('尚未安装离线 OCR 语言包，请在 Windows 设置的语言选项中安装文字识别组件。')
      }
      const data = kind === 'pip' ? '' : cropped(image, region, marks, kind === 'ocr' ? 2400 : undefined).toDataURL('image/png')
      const result = await screens.output(kind, data, { x: bounds.x + region.x, y: bounds.y + region.y, width: region.width, height: region.height }, language)
      if (kind === 'ocr') {
        setOcrText(result.text ?? '')
        if (!result.text?.trim()) setMessage('未识别到文字，可调整选区或切换语言后重试。')
      } else if (!result.canceled) {
        close()
      }
    } catch (e) { setError(String(e)) } finally { busyRef.current = false; setBusy('') }
  }

  useEffect(() => {
    const key = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.preventDefault()
        if (text) setText(null)
        else if (ocrOpen) setOcrOpen(false)
        else close()
        return
      }
      if ((e.target as HTMLElement).closest('input, textarea, select')) return
      if (busy) return
      if (e.key === 'Enter' || ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'c')) { e.preventDefault(); void output('copy') }
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 's') { e.preventDefault(); void output('save') }
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'z') { e.preventDefault(); if (e.shiftKey) redoMark(); else undo() }
      if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'y') { e.preventDefault(); redoMark() }
    }
    window.addEventListener('keydown', key)
    return () => window.removeEventListener('keydown', key)
  })

  const point = (e: PointerEvent): XY => physicalPoint({ x: e.clientX, y: e.clientY }, { width: window.innerWidth, height: window.innerHeight }, bounds)
  const relative = (p: XY, rect: Rect) => clampPoint({ x: p.x - rect.x, y: p.y - rect.y }, rect.width, rect.height)
  const start = (e: PointerEvent<HTMLDivElement>) => {
    if (!image || busy || (e.target as HTMLElement).closest('[data-ui]')) return
    if (e.button !== 0) return
    const p = point(e), handle = (e.target as HTMLElement).dataset.handle
    if (selection && handle) drag.current = { kind: 'resize', start: p, base: selection, handle }
    else if (!selection) drag.current = { kind: 'select', start: p }
    else if (p.x >= selection.x && p.x <= selection.x + selection.width && p.y >= selection.y && p.y <= selection.y + selection.height) {
      if (mode === 'select') drag.current = { kind: 'move', start: p, base: selection }
      else if (mode === 'text') { setText({ point: relative(p, selection), value: '' }); return }
      else {
        const mark: Mark = { kind: mode, points: [relative(p, selection)], color, width }
        drag.current = { kind: 'draw', start: p, mark }; setPending(mark)
      }
    } else { setSelection(null); setMarks([]); setRedo([]); setText(null); drag.current = { kind: 'select', start: p } }
    e.currentTarget.setPointerCapture(e.pointerId)
    setInteracting(true)
  }
  const move = (e: PointerEvent<HTMLDivElement>) => {
    const active = drag.current
    if (!active) return
    const p = point(e)
    if (active.kind === 'select') setSelection(rectangle(active.start, p))
    if (active.kind === 'move' && active.base) {
      const b = active.base
      setSelection({ ...b, x: Math.round(Math.max(0, Math.min(bounds.width - b.width, b.x + p.x - active.start.x))), y: Math.round(Math.max(0, Math.min(bounds.height - b.height, b.y + p.y - active.start.y))) })
    }
    if (active.kind === 'resize' && active.base) {
      const b = active.base, h = active.handle ?? ''
      const a = { x: h.includes('w') ? p.x : b.x, y: h.includes('n') ? p.y : b.y }
      const z = { x: h.includes('e') ? p.x : b.x + b.width, y: h.includes('s') ? p.y : b.y + b.height }
      setSelection(rectangle(a, z)); setMarks([]); setRedo([])
    }
    if (active.kind === 'draw' && active.mark && selection) {
      const mark = active.mark, next = relative(p, selection)
      mark.points = mark.kind === 'pen' ? [...mark.points, next] : [mark.points[0], next]
      setPending({ ...mark })
    }
  }
  const finish = (e: PointerEvent<HTMLDivElement>) => {
    const active = drag.current
    drag.current = null
    setInteracting(false)
    if (e.currentTarget.hasPointerCapture(e.pointerId)) e.currentTarget.releasePointerCapture(e.pointerId)
    if (!active) return
    if (active.kind === 'draw' && active.mark && active.mark.points.length > 1) { setMarks([...marks, active.mark]); setRedo([]) }
    setPending(null)
    const rect = active.kind === 'select' ? rectangle(active.start, point(e)) : selection
    if (rect && rect.width >= 2 && rect.height >= 2) {
      if (active.kind === 'select') {
        setSelection(rect)
        if (session.tool !== 'screenshot') void output(session.tool, rect)
      }
    } else if (active.kind === 'select' || active.kind === 'resize') setSelection(null)
  }
  const position = selection ? { left: `${selection.x / bounds.width * 100}%`, top: `${selection.y / bounds.height * 100}%`, width: `${selection.width / bounds.width * 100}%`, height: `${selection.height / bounds.height * 100}%` } : undefined

  return <div className={`capture-root mode-${mode}`} onPointerDown={start} onPointerMove={move} onPointerUp={finish} onPointerCancel={() => { drag.current = null; setPending(null); setInteracting(false) }} onContextMenu={e => { e.preventDefault(); if (!busy) close() }}>
    <canvas className="capture-background" ref={background} width={bounds.width} height={bounds.height} />
    {!selection && <div className="capture-shade" />}
    {!selection && <div className="capture-instructions" data-ui><strong>{TOOL_NAMES[session.tool]}</strong><span>拖动框选区域 · 右键或 Esc 取消</span><button title="取消截图" onClick={close}><X size={18} /></button></div>}
    {selection && <div className="capture-selection" style={position}>
      <canvas ref={preview} width={selection.width || 1} height={selection.height || 1} />
      <span className="capture-dimensions">{selection.width} × {selection.height}</span>
      {['nw', 'n', 'ne', 'e', 'se', 's', 'sw', 'w'].map(handle => <i key={handle} className={`selection-handle handle-${handle}`} data-handle={handle} />)}
    </div>}
    {selection && !interacting && <div className="capture-toolbar" data-ui onContextMenu={e => e.stopPropagation()}>
      {MODES.map(([key, label, Icon]) => <button key={key} className={mode === key ? 'active' : ''} title={label} aria-label={label} disabled={!!busy} onClick={() => { setMode(key); setText(null) }}><Icon size={17} /></button>)}
      <input type="color" aria-label="标注颜色" value={color} onChange={e => setColor(e.target.value)} />
      <select aria-label="线条粗细" value={width} onChange={e => setWidth(Number(e.target.value))}><option value="2">细</option><option value="4">中</option><option value="8">粗</option></select>
      <span className="toolbar-separator" />
      <button title="撤销 Ctrl+Z" aria-label="撤销" disabled={!marks.length || !!busy} onClick={undo}><Undo2 size={17} /></button>
      <button title="重做 Ctrl+Y" aria-label="重做" disabled={!redo.length || !!busy} onClick={redoMark}><Redo2 size={17} /></button>
      <span className="toolbar-separator" />
      <button title="文字识别" disabled={!!busy} onClick={() => void output('ocr')}><ScanText size={17} />识别</button>
      <button title="贴图置顶" disabled={!!busy} onClick={() => void output('pin')}><Pin size={17} />贴图</button>
      <button title="画中画" disabled={!!busy} onClick={() => void output('pip')}><PictureInPicture2 size={17} /></button>
      <button title="保存 Ctrl+S" aria-label="保存截图" disabled={!!busy} onClick={() => void output('save')}><Save size={17} /></button>
      <button className="primary" title="复制 Enter" aria-label="复制截图" disabled={!!busy} onClick={() => void output('copy')}><Copy size={17} />复制</button>
      <button title="取消截图" onClick={close}><X size={17} /></button>
    </div>}
    {text && selection && <div className="capture-text-entry" data-ui>
      <textarea autoFocus aria-label="标注文字" placeholder="输入标注文字" value={text.value} onChange={e => setText({ ...text, value: e.target.value })} />
      <button onClick={() => { if (text.value.trim()) { setMarks([...marks, { kind: 'text', points: [text.point], color, width, text: text.value }]); setRedo([]) } setText(null) }}><Check size={14} />添加文字</button>
      <button onClick={() => setText(null)}>取消</button>
    </div>}
    {ocrOpen && <section className="ocr-panel" data-ui onContextMenu={e => e.stopPropagation()}>
      <header><strong>离线文字识别</strong><button title="关闭识别面板" onClick={() => setOcrOpen(false)}><X size={16} /></button></header>
      <select aria-label="识别语言" value={language} onChange={e => setLanguage(e.target.value)}><option value="">系统首选语言</option>{languages.map(l => <option key={l.tag} value={l.tag}>{l.name}</option>)}</select>
      <textarea aria-label="识别结果" value={ocrText} placeholder="识别结果会显示在这里，可直接编辑" onChange={e => setOcrText(e.target.value)} />
      <footer><button disabled={!!busy} onClick={() => void output('ocr')}>重新识别</button><button className="primary" disabled={!ocrText.trim()} onClick={() => void screens.copyText(ocrText).then(() => setMessage('文字已复制')).catch(e => setError(String(e)))}><Copy size={15} />复制文字</button></footer>
    </section>}
    {(busy || error || message) && <div className={error ? 'capture-notice error' : 'capture-notice'} role={error ? 'alert' : 'status'} data-ui>{busy || error || message}{error && <button onClick={() => setError('')}>关闭提示</button>}</div>}
  </div>
}
