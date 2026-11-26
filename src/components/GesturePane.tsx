import { useEffect, useRef, useState } from 'react'
import { listen } from '@tauri-apps/api/event'
import {
  Check,
  Circle,
  Copy,
  Keyboard,
  Power,
  PowerOff,
  RotateCcw,
  Spline,
  Trash2,
  TriangleAlert,
  X,
} from 'lucide-react'
import {
  api,
  DOCS_GESTURE_URL,
  DOCS_URL,
  hotkeyLabel,
  isHotkeyOnly,
  openExternal,
  REGIONS,
  TRIGGERS,
  type Gesture,
  type LivePayload,
  type Point,
  type RecordedPayload,
  type TriggerButton,
} from '../api'
import { t, type Key } from '../i18n'
import { ActionEditor } from './ActionEditor'
import { HotkeyRecorder } from './HotkeyRecorder'
import { StrokeCanvas } from './StrokeCanvas'

interface Props {
  aiActions: import('../api').AiAction[]
  initial: Gesture
  isNew: boolean
  /** 保存失败时 reject，错误文案显示在面板内 */
  onSave: (gesture: Gesture) => Promise<void>
  onClose: () => void
  onToggle: (gesture: Gesture) => void
  onDelete: (gesture: Gesture) => void
  /** 以当前面板内容为模板新建手势 */
  onDuplicate: (gesture: Gesture) => void
  /** 被其他程序占用、注册失败的快捷键标签 */
  hotkeyFailures: string[]
}

const BUTTON_LABEL: Record<TriggerButton, Key> = {
  Right: 'btnRight',
  Middle: 'btnMiddle',
  X1: 'btnX1',
  X2: 'btnX2',
}

/** 选了屏幕边角后的触发方式：滚轮需要录制，单击 / 停留直接配置 */
type EdgeMode = 'wheel' | 'dwell' | `click:${TriggerButton}`
type TriggerTab = 'mouse' | 'hotkey'

const DEFAULT_DWELL_MS = 600

export function GesturePane({
  initial,
  aiActions,
  isNew,
  onSave,
  onClose,
  onToggle,
  onDelete,
  onDuplicate,
  hotkeyFailures,
}: Props) {
  const [draft, setDraft] = useState<Gesture>(() => ({ ...initial, hotkey: initial.hotkey ?? [] }))
  const [tab, setTab] = useState<TriggerTab>(() => (isHotkeyOnly(initial) ? 'hotkey' : 'mouse'))
  const [recording, setRecording] = useState(false)
  const [sequenceRecording, setSequenceRecording] = useState(false)
  const [keyRecording, setKeyRecording] = useState(false)
  const [saving, setSaving] = useState(false)
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

  const busy = recording || sequenceRecording || keyRecording

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

  const cancelRecord = () => {
    setRecording(false)
    liveRef.current = null
    setLive(null)
    void api.cancelRecording()
  }

  const clearMouse = () => {
    setError('')
    setDraft({
      ...draft,
      trigger: draft.trigger ?? 'Right',
      region: null,
      wheel: [],
      left_click: false,
      dwell_ms: 0,
      points: [],
      times: [],
    })
  }

  const hotkey = draft.hotkey ?? []
  const hotkeySet = hotkey.length > 0
  const recorded = draft.points.length > 0 || draft.wheel.length > 0
  const edgeMode: EdgeMode = draft.dwell_ms > 0 ? 'dwell' : draft.trigger ? `click:${draft.trigger}` : 'wheel'
  /** 边角单击 / 停留不需要录制，也没有「画到就执行」的区别 */
  const edgeNoRecord = !!draft.region && edgeMode !== 'wheel'
  const mouseSet = recorded || edgeNoRecord
  const onReleaseOnly =
    draft.left_click ||
    edgeNoRecord ||
    ['screenshot', 'record_region', 'ai_jyutping', 'ai_translate', 'ai_polish', 'ai_custom'].includes(draft.action.kind)
  const hotkeyBusy = hotkeySet && hotkeyFailures.includes(hotkeyLabel(hotkey))

  const setEdgeMode = (mode: EdgeMode) => {
    const cleared = { ...draft, wheel: [], points: [], times: [], left_click: false }
    if (mode === 'wheel') setDraft({ ...draft, trigger: null, dwell_ms: 0 })
    else if (mode === 'dwell') setDraft({ ...cleared, trigger: null, dwell_ms: draft.dwell_ms || DEFAULT_DWELL_MS })
    else setDraft({ ...cleared, trigger: mode.slice(6) as TriggerButton, dwell_ms: 0 })
  }

  const pickButton = (b: TriggerButton) =>
    setDraft({ ...draft, trigger: b, region: null, dwell_ms: 0, left_click: b === 'Right' && draft.left_click })

  const pickRegion = (v: string) => {
    if (!v) {
      setDraft({ ...draft, trigger: draft.trigger ?? 'Right', region: null, dwell_ms: 0 })
      return
    }
    const region = v as NonNullable<Gesture['region']>
    setDraft(
      draft.region
        ? { ...draft, region } // 换个边角，保留原来的边角动作
        : { ...draft, trigger: null, region, left_click: false, dwell_ms: 0 },
    )
  }

  const submit = async () => {
    const name = draft.name.trim()
    if (!name) return setError(t('needName'))
    if (draft.action.kind === 'launch_app' && !draft.action.app_id) return setError(t('needInstalledApp'))
    if (!mouseSet && !hotkeySet) return setError(t('needAnyTrigger'))

    let out: Gesture
    if (mouseSet) {
      if (!draft.trigger && !draft.region) return setError(t('needTrigger'))
      if (draft.region && draft.points.length > 0) return setError(t('regionWheelOnly'))
      if (draft.region && edgeMode === 'dwell' && !(draft.dwell_ms >= 100 && draft.dwell_ms <= 10000))
        return setError(t('dwellRange'))
      out = { ...draft, name, hotkey, dwell_ms: draft.region ? draft.dwell_ms : 0 }
    } else {
      // 只有快捷键：清空鼠标触发，与后端约定一致
      out = {
        ...draft,
        name,
        hotkey,
        trigger: null,
        region: null,
        wheel: [],
        left_click: false,
        dwell_ms: 0,
        points: [],
        times: [],
      }
    }

    setError('')
    setSaving(true)
    try {
      await onSave(out)
    } catch (e) {
      setError(String(e))
    } finally {
      setSaving(false)
    }
  }

  const docLink = (label: Key, url: string) => (
    <button
      type="button"
      className="link-inline"
      onClick={() => void openExternal(url).catch((e) => setError(String(e)))}
    >
      {t(label)}
    </button>
  )

  const status = (set: boolean, label: Key) => (
    <span className={set ? 'tab-status' : 'tab-status none'}>{set ? t(label) : t('stUnset')}</span>
  )

  return (
    <aside className="pane">
      <div className="pane-head">
        <input
          className="pane-name"
          type="text"
          value={draft.name}
          aria-label={t('name')}
          placeholder={isNew ? t('untitled') : t('namePlaceholder')}
          autoFocus={isNew}
          onChange={(e) => setDraft({ ...draft, name: e.target.value })}
          onKeyDown={(e) => e.key === 'Enter' && !busy && !saving && void submit()}
        />
        <button type="button" className="btn-icon" onClick={onClose} title={t('cancel')}>
          <X size={16} strokeWidth={2} />
        </button>
      </div>

      <div className="pane-body">
        <div className="trig">
          <div className="trig-tabs" role="tablist">
            <button
              type="button"
              role="tab"
              aria-selected={tab === 'mouse'}
              className={tab === 'mouse' ? 'on' : ''}
              disabled={busy && tab !== 'mouse'}
              onClick={() => setTab('mouse')}
            >
              <Spline size={15} strokeWidth={2} />
              {t('tabMouse')}
              {status(mouseSet, 'stRecorded')}
            </button>
            <button
              type="button"
              role="tab"
              aria-selected={tab === 'hotkey'}
              className={tab === 'hotkey' ? 'on' : ''}
              disabled={busy && tab !== 'hotkey'}
              onClick={() => setTab('hotkey')}
            >
              <Keyboard size={15} strokeWidth={2} />
              {t('tabHotkey')}
              {status(hotkeySet, 'stSet')}
            </button>
          </div>

          {tab === 'mouse' ? (
            <div className="trig-body" role="tabpanel">
              <div className={recording ? 'trig-stage live' : 'trig-stage'}>
                {live ? (
                  <StrokeCanvas
                    className="trig-canvas"
                    points={live.points}
                    trigger={live.trigger}
                    leftClick={live.left_click}
                    live
                    region={live.region}
                    wheel={live.wheel}
                    color="#6fc2ff"
                  />
                ) : mouseSet || draft.region ? (
                  <StrokeCanvas
                    className="trig-canvas"
                    points={draft.points}
                    trigger={draft.trigger}
                    leftClick={draft.left_click}
                    times={draft.times}
                    region={draft.region}
                    wheel={draft.wheel}
                    color="#6fc2ff"
                  />
                ) : recording ? null : (
                  <span className="trig-stage-empty">
                    {t('noStrokeYet')}
                    <br />
                    {t('noStrokeYetHint')}
                  </span>
                )}
                <span className="trig-stage-acts">
                  {recording ? (
                    <button type="button" className="btn-outline btn-xs" onClick={cancelRecord}>
                      <X size={13} strokeWidth={2} />
                      {t('cancel')}
                    </button>
                  ) : (
                    <>
                      <button
                        type="button"
                        className={mouseSet ? 'btn-outline btn-xs' : 'btn-primary btn-xs'}
                        onClick={startRecord}
                        disabled={busy}
                      >
                        {mouseSet ? <RotateCcw size={13} strokeWidth={2} /> : <Circle size={11} strokeWidth={3} />}
                        {mouseSet ? t('reRecord') : t('record')}
                      </button>
                      {(mouseSet || draft.region) && (
                        <button type="button" className="btn-outline btn-xs" onClick={clearMouse} disabled={busy}>
                          {t('clearKeys')}
                        </button>
                      )}
                    </>
                  )}
                </span>
              </div>
              {recording && <p className="hint">{t('recording')}</p>}

              <div className="fld">
                <span className="lbl">{t('trigger')}</span>
                <div className="chips">
                  {TRIGGERS.map((b) => (
                    <button
                      key={b}
                      type="button"
                      className={!draft.region && draft.trigger === b ? 'chip on' : 'chip'}
                      aria-pressed={!draft.region && draft.trigger === b}
                      onClick={() => pickButton(b)}
                    >
                      {t(BUTTON_LABEL[b])}
                    </button>
                  ))}
                  <select
                    className={draft.region ? 'chip chip-select on' : 'chip chip-select'}
                    aria-label={t('screenEdge')}
                    value={draft.region ?? ''}
                    onChange={(e) => pickRegion(e.target.value)}
                  >
                    <option value="">{t('screenEdge')} ▾</option>
                    {REGIONS.map((r) => (
                      <option key={r} value={r}>
                        {t(`region${r}`)}
                      </option>
                    ))}
                  </select>
                </div>
              </div>

              {draft.region && (
                <div className="fld-row">
                  <div className="fld">
                    <label className="lbl" htmlFor="g-edge">
                      {t('edgeMode')}
                    </label>
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
                  {edgeMode === 'dwell' && (
                    <div className="fld">
                      <label className="lbl" htmlFor="g-dwell">
                        {t('dwellMs')}
                      </label>
                      <input
                        id="g-dwell"
                        type="number"
                        min={100}
                        max={10000}
                        step={50}
                        value={draft.dwell_ms}
                        onChange={(e) => setDraft({ ...draft, dwell_ms: Math.round(Number(e.target.value)) || 0 })}
                      />
                    </div>
                  )}
                </div>
              )}

              <div className="fld">
                <span className="lbl">{t('fireMode')}</span>
                <div className="chips">
                  {(['OnRelease', 'Immediate'] as const).map((mode) => {
                    const on = (onReleaseOnly ? 'OnRelease' : draft.fire_mode) === mode
                    return (
                      <button
                        key={mode}
                        type="button"
                        className={on ? 'chip on' : 'chip'}
                        aria-pressed={on}
                        title={mode === 'OnRelease' ? t('onRelease') : t('immediate')}
                        disabled={onReleaseOnly && mode === 'Immediate'}
                        onClick={() => setDraft({ ...draft, fire_mode: mode })}
                      >
                        {mode === 'OnRelease' ? t('onReleaseShort') : t('immediateShort')}
                      </button>
                    )
                  })}
                </div>
                {draft.left_click && <p className="hint">{t('leftClickFireNote')}</p>}
              </div>

              <p className="hint">
                {edgeNoRecord ? t('edgeNoRecordHint') : t('recordHintShort')} {docLink('gestureGuide', DOCS_GESTURE_URL)}
              </p>
            </div>
          ) : (
            <div className="trig-body" role="tabpanel">
              <HotkeyRecorder
                variant="stage"
                keys={hotkey}
                onChange={(keys) => {
                  setError('')
                  setDraft((d) => ({ ...d, hotkey: keys }))
                }}
                onRecordingChange={setKeyRecording}
                disabled={busy}
              />
              {hotkeyBusy && (
                <p className="pane-banner" role="alert">
                  <TriangleAlert size={14} strokeWidth={2} />
                  {t('hotkeyBusy')}
                </p>
              )}
              <p className="hint">
                {t('hotkeyTriggerHint')} {docLink('hotkeyGuide', DOCS_URL)}
              </p>
            </div>
          )}
        </div>

        <div className="pane-sect-line">{t('actionShared')}</div>
        <ActionEditor aiActions={aiActions}
          action={draft.action}
          onChange={(action) => setDraft((current) => ({ ...current, action }))}
          onError={setError}
          onRecordingChange={setSequenceRecording}
          recordDisabled={busy}
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
              disabled={busy}
              title={t('duplicate')}
              onClick={() => onDuplicate({ ...draft, name: draft.name.trim() })}
            >
              <Copy size={14} strokeWidth={2} />
              {t('duplicate')}
            </button>
          </>
        )}
        <span className="grow" />
        <button type="button" className="btn-primary btn-sm" onClick={() => void submit()} disabled={busy || saving}>
          <Check size={15} strokeWidth={2} />
          {t('save')}
        </button>
      </div>
    </aside>
  )
}

/** 列表 / 卡片里只有快捷键的手势，用键盘图标代替轨迹缩略图 */
export function HotkeyThumb({ className }: { className: string }) {
  return (
    <span className={`${className} kb-thumb`} role="img" aria-label={t('hotkeyOnly')}>
      <Keyboard size={22} strokeWidth={1.8} />
    </span>
  )
}
