import { useEffect, useRef, useState } from 'react'
import { listen } from '@tauri-apps/api/event'
import {
  Check,
  MousePointerClick,
  Power,
  PowerOff,
  RotateCcw,
  Trash2,
  X,
} from 'lucide-react'
import {
  api,
  REGIONS,
  TRIGGERS,
  type FireMode,
  type Gesture,
  type LivePayload,
  type Point,
  type RecordedPayload,
} from '../api'
import { t } from '../i18n'
import { ActionEditor } from './ActionEditor'
import { StrokeCanvas } from './StrokeCanvas'

interface Props {
  initial: Gesture
  isNew: boolean
  onSave: (gesture: Gesture) => void
  onClose: () => void
  onToggle: (gesture: Gesture) => void
  onDelete: (gesture: Gesture) => void
}

const TRIGGER_LABEL: Record<string, string> = {
  Right: '鼠标右键',
  Middle: '鼠标中键',
  X1: '侧键 1',
  X2: '侧键 2',
}

export function GesturePane({ initial, isNew, onSave, onClose, onToggle, onDelete }: Props) {
  const [draft, setDraft] = useState<Gesture>(initial)
  const [recording, setRecording] = useState(false)
  const [live, setLive] = useState<LivePayload | null>(null)
  const [error, setError] = useState('')
  const liveRef = useRef<LivePayload | null>(null)
  const rafRef = useRef(0)

  useEffect(() => {
    /** 鼠标事件频率远高于刷新率，合并到每帧刷一次 */
    const schedule = () => {
      if (rafRef.current) return
      rafRef.current = requestAnimationFrame(() => {
        rafRef.current = 0
        const l = liveRef.current
        setLive(l && { ...l, points: [...l.points] })
      })
    }
    const stopLive = () => {
      liveRef.current = null
      cancelAnimationFrame(rafRef.current)
      rafRef.current = 0
      setLive(null)
    }
    const offs = [
      listen<LivePayload>('gesture:live', (e) => {
        liveRef.current = e.payload
        schedule()
      }),
      listen<Point>('gesture:live-point', (e) => {
        liveRef.current?.points.push(e.payload)
        schedule()
      }),
      listen<RecordedPayload>('gesture:recorded', (e) => {
        setRecording(false)
        stopLive()
        setError('')
        setDraft((d) => ({
          ...d,
          trigger: e.payload.trigger,
          region: e.payload.region,
          wheel: e.payload.wheel,
          points: e.payload.points,
          times: e.payload.times,
        }))
      }),
      listen<string>('gesture:record-failed', (e) => {
        setRecording(false)
        stopLive()
        setError(e.payload)
      }),
    ]
    return () => {
      offs.forEach((p) => void p.then((off) => off()))
      void api.cancelRecording()
    }
  }, [])

  const startRecord = async () => {
    setError('')
    liveRef.current = null
    setLive(null)
    setRecording(true)
    try {
      await api.startRecording()
    } catch (e) {
      setRecording(false)
      setError(String(e))
    }
  }

  const recorded = draft.points.length > 0 || draft.wheel.length > 0

  const submit = () => {
    if (!draft.name.trim()) return setError(t('needName'))
    if (draft.points.length === 0 && draft.wheel.length === 0) return setError(t('needStroke'))
    if (!draft.trigger && !draft.region) return setError(t('needTrigger'))
    if (draft.region && draft.points.length > 0) return setError(t('regionWheelOnly'))
    onSave({ ...draft, name: draft.name.trim() })
  }

  return (
    <aside className="pane">
      <div className="pane-head">
        <h2>{isNew ? t('addGesture') : draft.name || t('editGesture')}</h2>
        <button type="button" className="btn-icon" onClick={onClose} title={t('cancel')}>
          <X size={16} strokeWidth={2} />
        </button>
      </div>

      <div className="pane-body">
        <div className={recording ? 'recorder live' : 'recorder'}>
          {live ? (
            <StrokeCanvas
              className="frame"
              points={live.points}
              region={live.region}
              wheel={live.wheel}
              color="#6fc2ff"
            />
          ) : (
            <StrokeCanvas
              className="frame"
              points={draft.points}
              times={draft.times}
              region={draft.region}
              wheel={draft.wheel}
              color="#6fc2ff"
            />
          )}
          <p className="hint">{recording ? t('recording') : t('recordHint')}</p>
          <button type="button" className="btn-outline" onClick={startRecord} disabled={recording}>
            {recorded ? (
              <RotateCcw size={16} strokeWidth={2} />
            ) : (
              <MousePointerClick size={16} strokeWidth={2} />
            )}
            {recorded ? t('reRecord') : t('record')}
          </button>
        </div>

        <div className="field">
          <label htmlFor="g-name">{t('name')}</label>
          <input
            id="g-name"
            type="text"
            value={draft.name}
            placeholder={t('namePlaceholder')}
            onChange={(e) => setDraft({ ...draft, name: e.target.value })}
          />
        </div>

        <div className="row field">
          <div>
            <label htmlFor="g-trigger">{t('trigger')}</label>
            <select
              id="g-trigger"
              value={draft.region ? `region:${draft.region}` : draft.trigger ?? ''}
              onChange={(e) => {
                const v = e.target.value
                setDraft(
                  v.startsWith('region:')
                    ? { ...draft, trigger: null, region: v.slice(7) as Gesture['region'] }
                    : { ...draft, trigger: v as Gesture['trigger'], region: null },
                )
              }}
            >
              {TRIGGERS.map((b) => (
                <option key={b} value={b}>
                  {TRIGGER_LABEL[b]}
                </option>
              ))}
              {REGIONS.map((r) => (
                <option key={r} value={`region:${r}`}>
                  {t(`region${r}`)}
                </option>
              ))}
            </select>
          </div>
          <div>
            <label htmlFor="g-fire">{t('fireMode')}</label>
            <select
              id="g-fire"
              value={draft.fire_mode}
              onChange={(e) => setDraft({ ...draft, fire_mode: e.target.value as FireMode })}
            >
              <option value="OnRelease">{t('onRelease')}</option>
              <option value="Immediate">{t('immediate')}</option>
            </select>
          </div>
        </div>

        <h3 className="pane-sect">{t('action')}</h3>
        <ActionEditor
          action={draft.action}
          onChange={(action) => setDraft({ ...draft, action })}
          onError={setError}
        />

        {error && <p className="error pane-error">{error}</p>}
      </div>

      <div className="pane-foot">
        {isNew ? (
          <button type="button" className="btn-ghost" onClick={onClose}>
            <X size={14} strokeWidth={2} />
            {t('cancel')}
          </button>
        ) : (
          <>
            <button type="button" className="btn-ghost" onClick={() => onToggle(initial)}>
              {initial.enabled ? <PowerOff size={14} strokeWidth={2} /> : <Power size={14} strokeWidth={2} />}
              {initial.enabled ? t('disable') : t('enable')}
            </button>
            <button
              type="button"
              className="btn-ghost danger"
              disabled={!initial.deletable}
              title={initial.deletable ? '' : t('notDeletable')}
              onClick={() => onDelete(initial)}
            >
              <Trash2 size={14} strokeWidth={2} />
              {t('remove')}
            </button>
          </>
        )}
        <span className="grow" />
        <button type="button" className="btn-primary btn-sm" onClick={submit}>
          <Check size={15} strokeWidth={2} />
          {t('save')}
        </button>
      </div>
    </aside>
  )
}
