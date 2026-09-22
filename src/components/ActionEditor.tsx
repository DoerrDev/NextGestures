import { Play } from 'lucide-react'
import { api, type Action } from '../api'
import { t } from '../i18n'
import { HotkeyRecorder } from './HotkeyRecorder'

interface Props {
  action: Action
  onChange: (action: Action) => void
  onError: (msg: string) => void
}

const KINDS: Action['kind'][] = ['none', 'command', 'hotkey', 'key_sequence', 'open_url', 'lua']

const KIND_LABEL: Record<Action['kind'], () => string> = {
  none: () => t('actionNone'),
  command: () => t('actionCommand'),
  hotkey: () => t('actionHotkey'),
  key_sequence: () => t('actionKeySequence'),
  open_url: () => t('actionOpenUrl'),
  lua: () => t('actionLua'),
}

const LUA_TEMPLATE = `-- ng.send_keys("ctrl+w")        发送组合键
-- ng.run(prog, {arg1, arg2})   启动进程（不显示控制台）
-- ng.selection()               取当前选中文字（模拟 Ctrl+C，自动还原剪贴板）
-- ng.url_encode(text)          URL 编码
-- ng.cursor()                  返回鼠标坐标 x, y
-- ng.exe()                     返回前台窗口进程名
-- ng.sleep(ms)                 等待毫秒（最多 10000）

-- 示例：用选中文字搜索，等页面打开后切到上一个标签
local q = ng.url_encode(ng.selection())
ng.run("cmd", {"/C", "start", "", "https://www.google.com/search?q=" .. q})
ng.sleep(2500)
ng.send_keys("alt+tab")
`

function blank(kind: Action['kind']): Action {
  switch (kind) {
    case 'command':
      return { kind, program: '', args: [], shell: false }
    case 'hotkey':
      return { kind, keys: [] }
    case 'key_sequence':
      return { kind, steps: [] }
    case 'open_url':
      return { kind, url: 'https://www.google.com/search?q={selection}' }
    case 'lua':
      return { kind, code: LUA_TEMPLATE }
    default:
      return { kind: 'none' }
  }
}

const parseKeys = (text: string) =>
  text
    .split('+')
    .map((s) => s.trim())
    .filter(Boolean)

export function ActionEditor({ action, onChange, onError }: Props) {
  const test = async () => {
    try {
      await api.testAction(action)
    } catch (e) {
      onError(String(e))
    }
  }

  return (
    <>
      <div className="field">
        <label htmlFor="action-kind">{t('actionKind')}</label>
        <select
          id="action-kind"
          value={action.kind}
          onChange={(e) => onChange(blank(e.target.value as Action['kind']))}
        >
          {KINDS.map((k) => (
            <option key={k} value={k}>
              {KIND_LABEL[k]()}
            </option>
          ))}
        </select>
      </div>

      {action.kind === 'command' && (
        <>
          <div className="field">
            <label htmlFor="action-program">{t('program')}</label>
            <input
              id="action-program"
              type="text"
              value={action.program}
              placeholder="notepad.exe"
              onChange={(e) => onChange({ ...action, program: e.target.value })}
            />
          </div>
          <div className="field">
            <label htmlFor="action-args">{t('args')}</label>
            <input
              id="action-args"
              type="text"
              value={action.args.join(' ')}
              onChange={(e) =>
                onChange({ ...action, args: e.target.value.split(' ').filter(Boolean) })
              }
            />
          </div>
          <label className="check field">
            <input
              type="checkbox"
              checked={action.shell}
              onChange={(e) => onChange({ ...action, shell: e.target.checked })}
            />
            {t('shell')}
          </label>
        </>
      )}

      {action.kind === 'hotkey' && (
        <div className="field">
          <label>{t('keys')}</label>
          <HotkeyRecorder keys={action.keys} onChange={(keys) => onChange({ ...action, keys })} />
        </div>
      )}

      {action.kind === 'key_sequence' && (
        <div className="field">
          <label htmlFor="action-steps">{t('steps')}</label>
          <textarea
            id="action-steps"
            value={action.steps.map((s) => s.join('+')).join('\n')}
            placeholder={'ctrl+c\nalt+tab\nctrl+v'}
            onChange={(e) =>
              onChange({
                ...action,
                steps: e.target.value
                  .split('\n')
                  .map(parseKeys)
                  .filter((s) => s.length > 0),
              })
            }
          />
        </div>
      )}

      {action.kind === 'open_url' && (
        <div className="field">
          <label htmlFor="action-url">{t('url')}</label>
          <input
            id="action-url"
            type="text"
            value={action.url}
            placeholder="https://www.google.com/search?q={selection}"
            onChange={(e) => onChange({ ...action, url: e.target.value })}
          />
          <p className="card-desc" style={{ whiteSpace: 'normal', marginTop: 6 }}>
            {t('urlHint')}
          </p>
        </div>
      )}

      {action.kind === 'lua' && (
        <div className="field">
          <label htmlFor="action-lua">{t('luaCode')}</label>
          <textarea
            id="action-lua"
            rows={14}
            value={action.code}
            onChange={(e) => onChange({ ...action, code: e.target.value })}
          />
          <p className="card-desc" style={{ whiteSpace: 'normal', marginTop: 6 }}>
            {t('luaHint')}
          </p>
        </div>
      )}

      {action.kind !== 'none' && (
        <button type="button" className="btn-outline subtle" onClick={test}>
          <Play size={16} strokeWidth={2} />
          {t('test')}
        </button>
      )}
    </>
  )
}

export function describeAction(action: Action): string {
  switch (action.kind) {
    case 'command':
      return `${action.program} ${action.args.join(' ')}`.trim()
    case 'hotkey':
      return action.keys.join('+')
    case 'key_sequence':
      return action.steps.map((s) => s.join('+')).join(' → ')
    case 'open_url':
      return action.url
    case 'lua':
      return action.code.split('\n')[0] ?? 'lua'
    default:
      return t('actionNone')
  }
}
