import { useEffect, useState } from 'react'
import { Keyboard, X } from 'lucide-react'
import { t } from '../i18n'

interface Props {
  keys: string[]
  onChange: (keys: string[]) => void
}

const MODIFIERS = ['Control', 'Alt', 'Shift', 'Meta', 'ContextMenu']
const CHIPS = ['ctrl', 'alt', 'shift', 'win']

const CODE_MAP: Record<string, string> = {
  Escape: 'esc',
  Enter: 'enter',
  NumpadEnter: 'enter',
  Tab: 'tab',
  Space: 'space',
  Backspace: 'backspace',
  Delete: 'delete',
  Insert: 'insert',
  Home: 'home',
  End: 'end',
  PageUp: 'pgup',
  PageDown: 'pgdn',
  ArrowUp: 'up',
  ArrowDown: 'down',
  ArrowLeft: 'left',
  ArrowRight: 'right',
  PrintScreen: 'printscreen',
  CapsLock: 'capslock',
  Minus: '-',
  Equal: '=',
  Comma: ',',
  Period: '.',
  Slash: '/',
  Semicolon: ';',
  Quote: "'",
  BracketLeft: '[',
  BracketRight: ']',
  Backslash: '\\',
  Backquote: '`',
}

/** 把浏览器 keycode 转成后端 parse_key 认识的名字 */
function mainKey(code: string): string | null {
  if (code.startsWith('Key')) return code.slice(3).toLowerCase()
  if (code.startsWith('Digit')) return code.slice(5)
  if (/^F([1-9]|1[0-9]|2[0-4])$/.test(code)) return code.toLowerCase()
  return CODE_MAP[code] ?? null
}

export function HotkeyRecorder({ keys, onChange }: Props) {
  const [recording, setRecording] = useState(false)
  const [mods, setMods] = useState<string[]>([])

  useEffect(() => {
    if (!recording) return

    const onKeyDown = (e: KeyboardEvent) => {
      e.preventDefault()
      e.stopPropagation()
      if (MODIFIERS.includes(e.key)) return
      const key = mainKey(e.code)
      if (!key) return
      // Win+方向键等系统快捷键会被 OS 抢走，Win 通过点击芯片预选，其余修饰键按住即可
      const held = {
        ctrl: e.ctrlKey,
        alt: e.altKey,
        shift: e.shiftKey,
        win: e.metaKey,
      }
      onChange([...CHIPS.filter((m) => held[m as keyof typeof held] || mods.includes(m)), key])
      setRecording(false)
    }

    const stop = () => setRecording(false)
    window.addEventListener('keydown', onKeyDown, true)
    window.addEventListener('blur', stop)
    return () => {
      window.removeEventListener('keydown', onKeyDown, true)
      window.removeEventListener('blur', stop)
    }
  }, [recording, onChange, mods])

  const toggleMod = (m: string) =>
    setMods((prev) => (prev.includes(m) ? prev.filter((x) => x !== m) : [...prev, m]))

  const startRecording = () => {
    setMods([])
    setRecording(true)
  }

  return (
    <div className="hotkey">
      <div className={recording ? 'hotkey-box live' : 'hotkey-box'}>
        {recording ? (
          <>
            {CHIPS.map((m) => (
              <button
                type="button"
                key={m}
                className={mods.includes(m) ? 'tag blue' : 'tag mute'}
                onClick={() => toggleMod(m)}
              >
                {m}
              </button>
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
