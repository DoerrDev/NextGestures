import { useEffect, useRef, useState } from 'react'
import { listen } from '@tauri-apps/api/event'
import { Circle, Keyboard, RotateCcw, X } from 'lucide-react'
import { api } from '../api'
import { t } from '../i18n'

interface Props {
  keys: string[]
  onChange: (keys: string[]) => void
  onRecordingChange?: (recording: boolean) => void
  /** stage：大键帽展示 + 右下角按钮，用于手势编辑区的「快捷键」触发 */
  variant?: 'inline' | 'stage'
  /** 其他录制进行中时禁止开始录制 */
  disabled?: boolean
}

const CHIPS = ['ctrl', 'alt', 'shift', 'win']

export function HotkeyRecorder({ keys, onChange, onRecordingChange, variant = 'inline', disabled = false }: Props) {
  const [recording, setRecording] = useState(false)
  const [mods, setMods] = useState<string[]>([])
  // 父组件每次渲染都会传新的 onChange，不能让它重启原生录制
  const onChangeRef = useRef(onChange)
  onChangeRef.current = onChange
  const onRecordingChangeRef = useRef(onRecordingChange)
  onRecordingChangeRef.current = onRecordingChange

  // 按键由后端的低级键盘钩子截获：它先于系统和其它软件的热键执行，
  // 所以 Win+方向键、被别的程序占用的 F1 等都能录到，且录制时不会触发它们
  useEffect(() => {
    if (!recording) return

    onRecordingChangeRef.current?.(true)
    let alive = true
    const listeners = Promise.all([
      listen<string[]>('hotkey:mods', (e) => setMods(e.payload)),
      listen<string[]>('hotkey:recorded', (e) => {
        onChangeRef.current(e.payload)
        setRecording(false)
      }),
    ])
    // 监听就绪后再装钩子，避免漏掉结果
    void listeners.then(() => {
      if (alive) void api.beginKeyRecording()
    })

    const stop = () => setRecording(false)
    window.addEventListener('blur', stop)
    return () => {
      alive = false
      onRecordingChangeRef.current?.(false)
      window.removeEventListener('blur', stop)
      void listeners.then((fns) => fns.forEach((off) => off()))
      void api.cancelKeyRecording()
    }
  }, [recording])

  const startRecording = () => {
    setMods([])
    setRecording(true)
  }

  if (variant === 'stage') {
    return (
      <div className={recording ? 'trig-stage live' : 'trig-stage'}>
        {recording ? (
          <span className="trig-stage-live">
            <span className="keycaps">
              {CHIPS.map((m) => (
                <span key={m} className={mods.includes(m) ? 'tag blue' : 'tag mute'}>
                  {m}
                </span>
              ))}
            </span>
            <span className="hotkey-hint">{t('pressKeys')}</span>
          </span>
        ) : keys.length > 0 ? (
          <Keycaps keys={keys} big />
        ) : (
          <span className="trig-stage-empty">
            {t('noHotkeyYet')}
            <br />
            {t('noHotkeyYetHint')}
          </span>
        )}
        <span className="trig-stage-acts">
          <button
            type="button"
            className={keys.length > 0 || recording ? 'btn-outline btn-xs' : 'btn-primary btn-xs'}
            disabled={disabled && !recording}
            onClick={() => (recording ? setRecording(false) : startRecording())}
          >
            {recording ? (
              <X size={13} strokeWidth={2} />
            ) : keys.length > 0 ? (
              <RotateCcw size={13} strokeWidth={2} />
            ) : (
              <Circle size={11} strokeWidth={3} />
            )}
            {recording ? t('cancel') : keys.length > 0 ? t('reRecord') : t('record')}
          </button>
          {keys.length > 0 && !recording && (
            <button type="button" className="btn-outline btn-xs" onClick={() => onChange([])}>
              {t('clearKeys')}
            </button>
          )}
        </span>
      </div>
    )
  }

  return (
    <div className="hotkey">
      <div className={recording ? 'hotkey-box live' : 'hotkey-box'}>
        {recording ? (
          <>
            {CHIPS.map((m) => (
              <span key={m} className={mods.includes(m) ? 'tag blue' : 'tag mute'}>
                {m}
              </span>
            ))}
            <span className="hotkey-hint">{t('pressKeys')}</span>
          </>
        ) : keys.length > 0 ? (
          keys.map((k, i) => (
            <span key={`${k}-${i}`} className="tag">
              {k}
            </span>
          ))
        ) : (
          <span className="hotkey-hint">{t('noKeys')}</span>
        )}
      </div>
      <button
        type="button"
        className="btn-outline subtle"
        disabled={disabled && !recording}
        onClick={() => (recording ? setRecording(false) : startRecording())}
      >
        <Keyboard size={16} strokeWidth={2} />
        {recording ? t('cancel') : keys.length > 0 ? t('reRecord') : t('recordKeys')}
      </button>
      {keys.length > 0 && !recording && (
        <button
          type="button"
          className="btn-icon"
          title={t('clearKeys')}
          onClick={() => onChange([])}
        >
          <X size={16} strokeWidth={2} />
        </button>
      )}
    </div>
  )
}

/** 键帽样式的组合键展示：Ctrl + Alt + A */
export function Keycaps({ keys, big = false }: { keys: string[]; big?: boolean }) {
  return (
    <span className={big ? 'keycaps big' : 'keycaps'}>
      {keys.map((k, i) => (
        <span key={`${k}-${i}`} className="keycap-group">
          {i > 0 && <span className="keycap-plus">+</span>}
          <kbd>{k}</kbd>
        </span>
      ))}
    </span>
  )
}
