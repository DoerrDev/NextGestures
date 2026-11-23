import { Play } from 'lucide-react'
import { api, type Action, type Plugins } from '../api'
import { t, type Key } from '../i18n'
import { HotkeyRecorder } from './HotkeyRecorder'

interface Props {
  action: Action
  plugins: Plugins
  onChange: (action: Action) => void
  onError: (msg: string) => void
}

const PLUGIN_FOR_ACTION: Partial<Record<Action['kind'], 'screenshot' | 'recorder' | 'quickformater'>> = {
  screenshot: 'screenshot',
  record_region: 'recorder',
  ai_jyutping: 'quickformater',
  ai_translate: 'quickformater',
  ai_polish: 'quickformater',
}

const KIND_LABEL: Record<Action['kind'], Key> = {
  none: 'actionNone',
  command: 'actionCommand',
  hotkey: 'actionHotkey',
  key_sequence: 'actionKeySequence',
  open_url: 'actionOpenUrl',
  lua: 'actionLua',
  screenshot: 'actionScreenshot',
  record_region: 'actionRecordRegion',
  ai_jyutping: 'actionAiJyutping',
  ai_translate: 'actionAiTranslate',
  ai_polish: 'actionAiPolish',
}

const KINDS = Object.keys(KIND_LABEL) as Action['kind'][]

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
    case 'screenshot':
    case 'record_region':
    case 'ai_jyutping':
    case 'ai_translate':
    case 'ai_polish':
      return { kind }
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

export function ActionEditor({ action, plugins, onChange, onError }: Props) {
  const currentPlugin = PLUGIN_FOR_ACTION[action.kind]
  const isDisabled = currentPlugin !== undefined && !plugins[currentPlugin]

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
          {KINDS.map((kind) => {
            const plugin = PLUGIN_FOR_ACTION[kind]
            const disabled = plugin !== undefined && !plugins[plugin]
            return (
              <option key={kind} value={kind} disabled={disabled}>
                {t(KIND_LABEL[kind])}{disabled ? ` · ${t('pluginDisabled')}` : ''}
              </option>
            )
          })}
        </select>
      </div>

      {isDisabled && <p className="field-note">{t('pluginDisabledHint')}</p>}
      {action.kind === 'screenshot' && !isDisabled && (
        <p className="field-note">{t('screenshotActionHint')}</p>
      )}
      {action.kind === 'record_region' && !isDisabled && (
        <p className="field-note">{t('recordActionHint')}</p>
      )}
      {currentPlugin === 'quickformater' && !isDisabled && (
        <p className="field-note">{t('aiActionHint')}</p>
      )}

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

      {action.kind !== 'none' && currentPlugin === undefined && (
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
      return t(KIND_LABEL[action.kind])
  }
}
