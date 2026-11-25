import { useEffect, useRef, useState } from 'react'
import { listen } from '@tauri-apps/api/event'
import {
  Check,
  Copy,
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
  type TriggerButton,
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
  /** 以当前面板内容为模板新建手势 */
  onDuplicate: (gesture: Gesture) => void
}

const TRIGGER_LABEL: Record<string, string> = {
  Right: '鼠标右键',
  Middle: '鼠标中键',
  X1: '侧键 1',
  X2: '侧键 2',
}

/** 选了屏幕边角后的触发方式：滚轮需要录制，单击 / 停留直接配置 */
type EdgeMode = 'wheel' | 'dwell' | `click:${TriggerButton}`

const DEFAULT_DWELL_MS = 600

export function GesturePane({ initial, isNew, onSave, onClose, onToggle, onDelete, onDuplicate }: Props) {
  const [draft, setDraft] = useState<Gesture>(initial)
  const [recording, setRecording] = useState(false)
  const [sequenceRecording, setSequenceRecording] = useState(false)
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
          left_click: e.payload.left_click,
          dwell_ms: 0,
          fire_mode: e.payload.left_click ? 'OnRelease' : d.fire_mode,
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
  const edgeMode: EdgeMode = draft.dwell_ms > 0 ? 'dwell' : draft.trigger ? `click:${draft.trigger}` : 'wheel'
  /** 边角单击 / 停留不需要录制，也没有「画到就执行」的区别 */
  const edgeNoRecord = !!draft.region && edgeMode !== 'wheel'
  const onReleaseOnly = draft.left_click || edgeNoRecord || ['screenshot', 'record_region', 'ai_jyutping', 'ai_translate', 'ai_polish'].includes(draft.action.kind)

  const setEdgeMode = (mode: EdgeMode) => {
    const cleared = { ...draft, wheel: [], points: [], times: [], left_click: false }
    if (mode === 'wheel') setDraft({ ...draft, trigger: null, dwell_ms: 0 })
    else if (mode === 'dwell') setDraft({ ...cleared, trigger: null, dwell_ms: draft.dwell_ms || DEFAULT_DWELL_MS })
    else setDraft({ ...cleared, trigger: mode.slice(6) as TriggerButton, dwell_ms: 0 })
  }

  const submit = () => {
    if (!draft.name.trim()) return setError(t('needName'))
    if (draft.action.kind === 'launch_app' && !draft.action.app_id) return setError(t('needInstalledApp'))
    if (!edgeNoRecord && draft.points.length === 0 && draft.wheel.length === 0) return setError(t('needStroke'))
    if (!draft.trigger && !draft.region) return setError(t('needTrigger'))
    if (draft.region && draft.points.length > 0) return setError(t('regionWheelOnly'))
    if (draft.region && edgeMode === 'dwell' && !(draft.dwell_ms >= 100 && draft.dwell_ms <= 10000))
      return setError(t('dwellRange'))
    onSave({ ...draft, name: draft.name.trim(), dwell_ms: draft.region ? draft.dwell_ms : 0 })
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
              trigger={live.trigger}
              leftClick={live.left_click}
              live
              region={live.region}
              wheel={live.wheel}
              color="#6fc2ff"
            />
          ) : (
            <StrokeCanvas
              className="frame"
              points={draft.points}
              trigger={draft.trigger}
              leftClick={draft.left_click}
              times={draft.times}
              region={draft.region}
              wheel={draft.wheel}
              color="#6fc2ff"
            />
          )}
          <p className="hint">{recording ? t('recording') : edgeNoRecord ? t('edgeNoRecordHint') : t('recordHint')}</p>
          <button type="button" className="btn-outline" onClick={startRecord} disabled={recording || sequenceRecording}>
            {recorded || edgeNoRecord ? (
              <RotateCcw size={16} strokeWidth={2} />
            ) : (
              <MousePointerClick size={16} strokeWidth={2} />
            )}
            {recorded || edgeNoRecord ? t('reRecord') : t('record')}
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
                const region = v.startsWith('region:') ? (v.slice(7) as Gesture['region']) : null
                setDraft(
                  region
                    ? draft.region
                      ? { ...draft, region } // 换个边角，保留原来的边角动作
                      : { ...draft, trigger: null, region, left_click: false, dwell_ms: 0 }
                    : { ...draft, trigger: v as Gesture['trigger'], region: null, dwell_ms: 0, left_click: v === 'Right' && draft.left_click },
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
              value={onReleaseOnly ? 'OnRelease' : draft.fire_mode}
              disabled={onReleaseOnly}
              onChange={(e) => setDraft({ ...draft, fire_mode: e.target.value as FireMode })}
            >
              <option value="OnRelease">{t('onRelease')}</option>
              <option value="Immediate">{t('immediate')}</option>
            </select>
            {draft.left_click && (
              <p className="field-note" style={{ marginTop: 6 }}>
                {t('leftClickFireNote')}
              </p>
            )}
          </div>
        </div>

        {draft.region && (
          <div className="row field">
            <div>
              <label htmlFor="g-edge">{t('edgeMode')}</label>
              <select id="g-edge" value={edgeMode} onChange={(e) => setEdgeMode(e.target.value as EdgeMode)}>
                <option value="wheel">{t('edgeWheel')}</option>
                {TRIGGERS.map((b) => (
                  <option key={b} value={`click:${b}`}>
                    {t(`edgeClick${b}`)}
                  </option>
                ))}
                <option value="dwell">{t('edgeDwell')}</option>
              </select>
            </div>
            <div>
              {edgeMode === 'dwell' && (
                <>
                  <label htmlFor="g-dwell">{t('dwellMs')}</label>
                  <input
                    id="g-dwell"
                    type="number"
                    min={100}
                    max={10000}
                    step={50}
                    value={draft.dwell_ms}
                    onChange={(e) => setDraft({ ...draft, dwell_ms: Math.round(Number(e.target.value)) || 0 })}
                  />
                </>
              )}
            </div>
          </div>
        )}

        <h3 className="pane-sect">{t('action')}</h3>
        <ActionEditor
          action={draft.action}
          onChange={(action) => setDraft((current) => ({ ...current, action }))}
          onError={setError}
          onRecordingChange={setSequenceRecording}
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
            <button
              type="button"
              className="btn-ghost"
              disabled={sequenceRecording}
              title={t('duplicate')}
              onClick={() => onDuplicate({ ...draft, name: draft.name.trim() })}
            >
              <Copy size={14} strokeWidth={2} />
              {t('duplicate')}
            </button>
          </>
        )}
        <span className="grow" />
        <button type="button" className="btn-primary btn-sm" onClick={submit} disabled={sequenceRecording}>
          <Check size={15} strokeWidth={2} />
          {t('save')}
        </button>
      </div>
    </aside>
  )
}
