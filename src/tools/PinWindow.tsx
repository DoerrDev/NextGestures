import { useState } from 'react'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { Copy, MousePointer2, Save, X } from 'lucide-react'
import { screens, type ScreenSession } from './api'

export function PinWindow({ session }: { session: ScreenSession }) {
  const [opacity, setOpacity] = useState(1)
  const [scale, setScale] = useState(Math.min(720 / session.bounds.width, 560 / session.bounds.height, 1))
  const [message, setMessage] = useState('')
  const [busy, setBusy] = useState(false)
  const close = () => void screens.control(session.label, 'close').catch(e => setMessage(String(e)))
  const output = async (kind: string) => {
    setBusy(true)
    try {
      const image = new Image(); image.crossOrigin = 'anonymous'; image.src = screens.image(session.label); await image.decode()
      const canvas = document.createElement('canvas'); canvas.width = image.naturalWidth; canvas.height = image.naturalHeight
      canvas.getContext('2d')!.drawImage(image, 0, 0)
      const result = await screens.output(kind, canvas.toDataURL('image/png'))
      if (!result.canceled) setMessage(kind === 'copy' ? '图片已复制' : '图片已保存')
    } catch (e) { setMessage(String(e)) } finally { setBusy(false) }
  }
  const resize = async (value: number) => {
    const next = Math.max(.1, Math.min(3, value)); setScale(next)
    try { await screens.control(session.label, 'resize', next) } catch (e) { setMessage(String(e)) }
  }
  return <div className="pin-window" onKeyDown={e => { if (e.key === 'Escape') close() }} tabIndex={-1}>
    <header className="pin-toolbar">
      <span className="pin-drag" onMouseDown={e => { if (e.button === 0) void getCurrentWindow().startDragging() }}>贴图</span>
      <button title="复制图片" disabled={busy} onClick={() => void output('copy')}><Copy size={14} /></button>
      <button title="保存图片" disabled={busy} onClick={() => void output('save')}><Save size={14} /></button>
      <label title="透明度"><input aria-label="透明度" type="range" min=".15" max="1" step=".05" value={opacity} onChange={e => setOpacity(Number(e.target.value))} /></label>
      <button title="鼠标穿透（从主界面截图工具中恢复交互）" onClick={() => void screens.control(session.label, 'interactive', 0).catch(e => setMessage(String(e)))}><MousePointer2 size={14} /></button>
      <button title="关闭贴图" onClick={close}><X size={14} /></button>
    </header>
    <img className="pin-image" src={screens.image(session.label)} draggable={false} style={{ opacity }} alt="置顶贴图，可拖动并用滚轮缩放" onMouseDown={e => { if (e.button === 0) void getCurrentWindow().startDragging() }} onWheel={e => { void resize(scale * (e.deltaY > 0 ? .9 : 1.1)) }} />
    {message && <div className="pin-message" role="status" onClick={() => setMessage('')}>{message}</div>}
  </div>
}
