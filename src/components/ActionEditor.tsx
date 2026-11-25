import { Play } from 'lucide-react'
import { api, type Action } from '../api'
import { t, type Key } from '../i18n'
import { HotkeyRecorder } from './HotkeyRecorder'
import { KeySequenceRecorder } from './KeySequenceRecorder'
import { ProgramPicker } from './ProgramPicker'

interface Props {
  action: Action
  onChange: (action: Action) => void
  onError: (msg: string) => void
  onRecordingChange: (recording: boolean) => void
}

type EditorKind = Exclude<Action['kind'], 'command'>

const KIND_LABEL: Record<EditorKind, Key> = {
  none: 'actionNone',
  launch_app: 'actionLaunchApp',
  hotkey: 'actionHotkey',
  key_sequence: 'actionKeySequence',
  open_url: 'actionOpenUrl',
  screenshot: 'actionScreenshot',
  record_region: 'actionRecordRegion',
  ai_jyutping: 'actionAiJyutping',
  ai_translate: 'actionAiTranslate',
  ai_polish: 'actionAiPolish',
}

const KINDS = Object.keys(KIND_LABEL) as EditorKind[]

function blank(kind: EditorKind): Action {
  switch (kind) {
    case 'screenshot':
    case 'record_region':
    case 'ai_jyutping':
    case 'ai_translate':
    case 'ai_polish':
      return { kind }
    case 'launch_app':
      return { kind, app_id: '', name: '' }
    case 'hotkey':
      return { kind, keys: [] }
    case 'key_sequence':
      return { kind }
    case 'open_url':
      return { kind, url: 'https://www.google.com/search?q={selection}' }
    default:
      return { kind: 'none' }
  }
}

export function ActionEditor({ action, onChange, onError, onRecordingChange }: Props) {
  const isAiAction = action.kind === 'ai_jyutping' || action.kind === 'ai_translate' || action.kind === 'ai_polish'
  const canTest = action.kind === 'launch_app' || action.kind === 'hotkey' || action.kind === 'open_url' || action.kind === 'command'

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
          onChange={(e) => onChange(blank(e.target.value as EditorKind))}
        >
          {action.kind === 'command' && (
            <option value="command" disabled>{t('legacyCommand')}</option>
          )}
          {KINDS.map((kind) => (
            <option key={kind} value={kind}>{t(KIND_LABEL[kind])}</option>
          ))}
        </select>
      </div>

      {action.kind === 'screenshot' && (
        <p className="field-note">{t('screenshotActionHint')}</p>
      )}
      {action.kind === 'record_region' && (
        <p className="field-note">{t('recordActionHint')}</p>
      )}
      {isAiAction && (
        <p className="field-note">{t('aiActionHint')}</p>
      )}

      {action.kind === 'command' && <p className="field-note">{t('legacyCommandHint')}</p>}
      {(action.kind === 'launch_app' || action.kind === 'command') && (
        <ProgramPicker
          selected={action.kind === 'launch_app' && action.app_id
            ? { id: action.app_id, name: action.name }
            : undefined}
          onSelect={(app) => onChange({ kind: 'launch_app', app_id: app.id, name: app.name })}
        />
      )}

      {action.kind === 'hotkey' && (
        <div className="field">
          <label>{t('keys')}</label>
          <HotkeyRecorder keys={action.keys} onChange={(keys) => onChange({ ...action, keys })} />
        </div>
      )}

      {action.kind === 'key_sequence' && (
        <KeySequenceRecorder
          action={action}
          onChange={onChange}
          onError={onError}
          onRecordingChange={onRecordingChange}
        />
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

      {canTest && (
        <button type="button" className="btn-outline subtle" onClick={test}
          disabled={action.kind === 'launch_app' && !action.app_id}>
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
    case 'launch_app':
      return action.name || t('actionLaunchApp')
    case 'hotkey':
      return action.keys.join('+')
    case 'key_sequence':
      return action.events?.length
        ? action.events.filter((e) => e.down).map((e) => e.label).join(' → ')
        : action.steps?.map((step) => step.join('+')).join(' → ') || t('actionKeySequence')
    case 'open_url':
      return action.url
    default:
      return t(KIND_LABEL[action.kind])
  }
}
