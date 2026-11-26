import { useState } from 'react'
import {
  AppWindow,
  Circle,
  CircleSlash,
  Ellipsis,
  ExternalLink,
  Keyboard,
  ListOrdered,
  Play,
  Scissors,
  type LucideIcon,
} from 'lucide-react'
import { api, type Action, type AiAction } from '../api'
import { t, type Key } from '../i18n'
import { aiIcon } from './aiIcons'
import { HotkeyRecorder } from './HotkeyRecorder'
import { KeySequenceRecorder } from './KeySequenceRecorder'
import { ProgramPicker } from './ProgramPicker'

interface Props {
  action: Action
  aiActions: AiAction[]
  onChange: (action: Action) => void
  onError: (msg: string) => void
  onRecordingChange: (recording: boolean) => void
  /** 编辑区内其他录制进行中，禁止开始新的按键录制 */
  recordDisabled?: boolean
}

type EditorKind = Exclude<Action['kind'], 'command' | 'ai_custom'>

const KIND_LABEL: Record<EditorKind, Key> = {
  none: 'actionNone',
  launch_app: 'actionLaunchApp',
  hotkey: 'actionHotkey',
  key_sequence: 'actionKeySequence',
  open_url: 'actionOpenUrl',
  screenshot: 'actionScreenshot',
  record_region: 'actionRecordRegion',
}

/** 图标网格里直接露出的常用动作，其余收在「更多」里 */
const COMMON_TILES: { kind: EditorKind; label: Key; icon: LucideIcon }[] = [
  { kind: 'hotkey', label: 'tileHotkey', icon: Keyboard },
  { kind: 'open_url', label: 'tileOpenUrl', icon: ExternalLink },
  { kind: 'launch_app', label: 'tileLaunchApp', icon: AppWindow },
  { kind: 'screenshot', label: 'tileScreenshot', icon: Scissors },
  { kind: 'record_region', label: 'tileRecordRegion', icon: Circle },
]

const MORE_TILES: { kind: EditorKind; label: Key; icon: LucideIcon }[] = [
  { kind: 'key_sequence', label: 'tileKeySequence', icon: ListOrdered },
  { kind: 'none', label: 'tileNone', icon: CircleSlash },
]

const isMoreKind = (kind: Action['kind']) => kind === 'command' || MORE_TILES.some((tile) => tile.kind === kind)

/** 自定义 AI 动作在主网格里最多露出的个数，其余收进「更多」 */
const INLINE_CUSTOM = 4

function KindTiles({ action, aiActions, onPick, onPickCustom }: {
  action: Action
  aiActions: AiAction[]
  onPick: (kind: EditorKind) => void
  onPickCustom: (id: string) => void
}) {
  const kind = action.kind
  const customIndex = action.kind === 'ai_custom' ? aiActions.findIndex((a) => a.id === action.id) : -1
  const [moreOpen, setMoreOpen] = useState(() => isMoreKind(kind) || customIndex >= INLINE_CUSTOM)
  const customTile = (a: AiAction) => {
    const Icon = aiIcon(a.icon)
    const on = action.kind === 'ai_custom' && action.id === a.id
    return (
      <button key={a.id} type="button" role="radio" aria-checked={on} className={on ? 'tile on' : 'tile'}
        title={a.name} onClick={() => !on && onPickCustom(a.id)}>
        <Icon size={16} strokeWidth={2} />
        <span className="tile-name">{a.name || '新动作'}</span>
      </button>
    )
  }
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
      {aiActions.slice(0, INLINE_CUSTOM).map(customTile)}
      <button
        type="button"
        className={moreOpen || isMoreKind(kind) || customIndex >= INLINE_CUSTOM ? 'tile on more' : 'tile more'}
        aria-expanded={moreOpen}
        onClick={() => setMoreOpen(!moreOpen)}
      >
        <Ellipsis size={16} strokeWidth={2} />
        {t('tileMore')}
      </button>
      {moreOpen && aiActions.slice(INLINE_CUSTOM).map(customTile)}
      {moreOpen && MORE_TILES.map(tile)}
    </div>
  )
}

function blank(kind: EditorKind): Action {
  switch (kind) {
    case 'screenshot':
    case 'record_region':
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

export function ActionEditor({ action, aiActions, onChange, onError, onRecordingChange, recordDisabled = false }: Props) {
  const isAiAction = action.kind === 'ai_custom'
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
      <KindTiles action={action} aiActions={aiActions} onPick={(kind) => onChange(blank(kind))}
        onPickCustom={(id) => onChange({ kind: 'ai_custom', id })} />
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

export function describeAction(action: Action, aiActions: AiAction[]): string {
  switch (action.kind) {
    case 'ai_custom': return aiActions.find(a => a.id === action.id)?.name || '已移除的 AI 动作'
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
