import { useEffect, useState } from 'react'
import { listen } from '@tauri-apps/api/event'
import { Camera, MousePointer2, PictureInPicture2, Pin, ScanText, Trash2, X } from 'lucide-react'
import { screens, TOOL_NAMES, type ScreenSession, type ScreenTool } from '../tools/api'

export function ScreenToolsPanel({ onClose }: { onClose: () => void }) {
  const [windows, setWindows] = useState<ScreenSession[]>([])
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const reload = () => screens.windows().then(setWindows).catch(e => setError(String(e)))
  useEffect(() => {
    void reload()
    const off = listen('screen:windows-changed', () => void reload())
    const timer = window.setInterval(() => void reload(), 2000)
    return () => { window.clearInterval(timer); void off.then(fn => fn()) }
  }, [])
  const start = async (tool: ScreenTool) => {
    setBusy(true); setError('')
    try { await screens.start(tool); onClose() } catch (e) { setError(String(e)) } finally { setBusy(false) }
  }
  const control = async (label: string, operation: string) => {
    try { await screens.control(label, operation, 1); await reload() } catch (e) { setError(String(e)) }
  }
  return <div className="mask" onMouseDown={e => { if (e.target === e.currentTarget && !busy) onClose() }}>
    <div className="dialog" role="dialog" aria-modal="true" aria-labelledby="screen-tools-title">
      <div className="dialog-head"><h2 id="screen-tools-title">截图工具</h2><button className="btn-icon" title="关闭截图工具" disabled={busy} onClick={onClose}><X size={16} /></button></div>
      <div className="dialog-body">
        <div className="screen-tool-grid">
          {([
            ['screenshot', Camera, '框选、标注、复制或保存'], ['pin', Pin, '静态贴图，拖动、缩放与透明度'],
            ['pip', PictureInPicture2, '窗口区域实时置顶预览'], ['ocr', ScanText, '本地中英文识别，可编辑并复制'],
          ] as const).map(([tool, Icon, description]) => <button key={tool} className="screen-tool-card" disabled={busy} onClick={() => void start(tool)}>
            <Icon size={22} /><strong>{TOOL_NAMES[tool]}</strong><span>{description}</span>
          </button>)}
        </div>
        <p className="blacklist-help" style={{ marginTop: 16 }}>也可在手势的“动作类型”中选择“截图工具”绑定使用。</p>
        {busy && <p role="status">正在准备桌面画面…</p>}
        {error && <p className="error" role="alert">{error}</p>}
        <div className="screen-window-heading">悬浮窗口 <small>{windows.length}</small></div>
        {windows.length === 0 ? <p className="blacklist-help">当前没有贴图或画中画窗口。</p> : <ul className="screen-window-list">
          {windows.map((win, i) => <li key={win.label}>
            {win.kind === 'pin' ? <Pin size={16} /> : <PictureInPicture2 size={16} />}
            <span>{win.kind === 'pin' ? '贴图' : '画中画'} {i + 1}{win.kind === 'pip_paused' ? ' · 已暂停' : ''}{win.click_through ? ' · 鼠标穿透' : ''}</span>
            {win.click_through && <button className="btn-outline subtle" onClick={() => void control(win.label, 'interactive')}><MousePointer2 size={14} />恢复交互</button>}
            <button className="btn-icon" title={`关闭${win.kind === 'pin' ? '贴图' : '画中画'} ${i + 1}`} onClick={() => void control(win.label, 'close')}><Trash2 size={16} /></button>
          </li>)}
        </ul>}
        <details className="screen-tools-help"><summary>使用说明</summary><p>框选时可右键或按 Esc 取消。截图支持矩形、箭头、画笔、文字、马赛克和撤销。贴图可用滚轮缩放；启用鼠标穿透后，在这里恢复交互。画中画会持续显示被遮挡的源窗口，最小化时暂停；源窗口关闭后自动结束。OCR 使用系统已安装的离线识别语言包。</p></details>
      </div>
      <div className="dialog-foot"><button className="btn-outline subtle" disabled={busy} onClick={onClose}>关闭</button></div>
    </div>
  </div>
}
