import { useState } from 'react'
import {
  AppWindow,
  Circle,
  CircleSlash,
  Ellipsis,
  ExternalLink,
  Keyboard,
  Languages,
  ListOrdered,
  PenLine,
  Play,
  Scissors,
  Type,
  type LucideIcon,
} from 'lucide-react'
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
  /** 编辑区内其他录制进行中，禁止开始新的按键录制 */
  recordDisabled?: boolean
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

/** 图标网格里直接露出的常用动作，其余收在「更多」里 */
const COMMON_TILES: { kind: EditorKind; label: Key; icon: LucideIcon }[] = [
  { kind: 'hotkey', label: 'tileHotkey', icon: Keyboard },
  { kind: 'open_url', label: 'tileOpenUrl', icon: ExternalLink },
  { kind: 'launch_app', label: 'tileLaunchApp', icon: AppWindow },
  { kind: 'screenshot', label: 'tileScreenshot', icon: Scissors },
  { kind: 'record_region', label: 'tileRecordRegion', icon: Circle },
  { kind: 'ai_translate', label: 'tileTranslate', icon: Languages },
  { kind: 'ai_polish', label: 'tilePolish', icon: PenLine },
]

const MORE_TILES: { kind: EditorKind; label: Key; icon: LucideIcon }[] = [
  { kind: 'key_sequence', label: 'tileKeySequence', icon: ListOrdered },
  { kind: 'ai_jyutping', label: 'tileJyutping', icon: Type },
  { kind: 'none', label: 'tileNone', icon: CircleSlash },
]

const isMoreKind = (kind: Action['kind']) => kind === 'command' || MORE_TILES.some((tile) => tile.kind === kind)

function KindTiles({ kind, onPick }: { kind: Action['kind']; onPick: (kind: EditorKind) => void }) {
  const [moreOpen, setMoreOpen] = useState(() => isMoreKind(kind))
  const tile = ({ kind: k, label, icon: Icon }: (typeof COMMON_TILES)[number]) => (
    <button
      key={k}
      type="button"
      role="radio"
      aria-checked={kind === k}
      className={kind === k ? 'tile on' : 'tile'}
      title={t(KIND_LABEL[k])}
      onClick={() => kind !== k && onPick(k)}
    >
      <Icon size={16} strokeWidth={2} />
      {t(label)}
    </button>
  )
  return (
    <div className="tiles" role="radiogroup" aria-label={t('actionKind')}>
      {COMMON_TILES.map(tile)}
      <button
        type="button"
        className={moreOpen || isMoreKind(kind) ? 'tile on more' : 'tile more'}
        aria-expanded={moreOpen}
        onClick={() => setMoreOpen(!moreOpen)}
      >
        <Ellipsis size={16} strokeWidth={2} />
        {t('tileMore')}
      </button>
      {moreOpen && MORE_TILES.map(tile)}
    </div>
  )
}

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

export function ActionEditor({ action, onChange, onError, onRecordingChange, recordDisabled = false }: Props) {
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
      <KindTiles kind={action.kind} onPick={(kind) => onChange(blank(kind))} />
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
          <HotkeyRecorder keys={action.keys} onChange={(keys) => onChange({ ...action, keys })}
            onRecordingChange={onRecordingChange} disabled={recordDisabled} />
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
