import { useEffect, useState } from 'react'
import { Check, Info, MessageCircle, MessageSquareText, MousePointer2, Pipette, Spline, X } from 'lucide-react'
import { api, TRIGGERS, type Point, type Settings, type TriggerButton, type VirtualScreenSize } from '../api'
import { t, type Key } from '../i18n'
import { HotkeyRecorder } from './HotkeyRecorder'
import { StrokeCanvas } from './StrokeCanvas'
import { FeedbackPane } from './FeedbackPane'

interface Props {
  initial: Settings
  configPath: string
  /** 来自开发者的未读回复数，用于页签红点 */
  unread: number
  onSave: (settings: Settings) => void
  onClose: () => void
}

const TRIGGER_LABEL: Record<TriggerButton, string> = {
  Right: '右键',
  Middle: '中键',
  X1: '侧键 1',
  X2: '侧键 2',
}

const PALETTE = [
  '#6fc2ff',
  '#383838',
  '#ffde00',
  '#ff8a5c',
  '#4fd1a5',
  '#b08cff',
  '#ff5d5d',
  '#ffffff',
]

/** 预览用的「2」字轨迹 */
const PREVIEW_STROKE: Point[] = (() => {
  const pts: Point[] = []
  for (let i = 0; i <= 40; i++) {
    const rad = ((200 - (215 * i) / 40) * Math.PI) / 180
    pts.push([50 + 42 * Math.cos(rad), 32 - 30 * Math.sin(rad)])
  }
  pts.push([12, 92], [95, 92])
  return pts
})()

type EyeDropperCtor = new () => { open: () => Promise<{ sRGBHex: string }> }

const TABS: { id: string; label: Key; icon: typeof Info }[] = [
  { id: 'trigger', label: 'tabTrigger', icon: MousePointer2 },
  { id: 'trail', label: 'tabTrail', icon: Spline },
  { id: 'hint', label: 'tabHint', icon: MessageSquareText },
  { id: 'feedback', label: 'tabFeedback', icon: MessageCircle },
  { id: 'about', label: 'tabAbout', icon: Info },
]

function ColorField({
  id,
  label,
  value,
  onChange,
}: {
  id: string
  label: string
  value: string
  onChange: (color: string) => void
}) {
  const EyeDropper = (window as unknown as { EyeDropper?: EyeDropperCtor }).EyeDropper

  const pick = async () => {
    if (!EyeDropper) return
    try {
      const { sRGBHex } = await new EyeDropper().open()
      onChange(sRGBHex)
    } catch {
      /* 用户取消 */
    }
  }

  return (
    <div className="field">
      <label htmlFor={id}>{label}</label>
      <div className="color-row">
        <input
          type="color"
          className="color-chip"
          value={/^#[0-9a-fA-F]{6}$/.test(value) ? value : '#6fc2ff'}
          onChange={(e) => onChange(e.target.value)}
        />
        <input id={id} type="text" value={value} onChange={(e) => onChange(e.target.value)} />
        {EyeDropper && (
          <button type="button" className="btn-icon" title={t('pickColor')} onClick={pick}>
            <Pipette size={16} strokeWidth={2} />
          </button>
        )}
      </div>
      <div className="swatches">
        {PALETTE.map((c) => (
          <button
            key={c}
            type="button"
            className={c.toLowerCase() === value.toLowerCase() ? 'swatch on' : 'swatch'}
            style={{ background: c }}
            title={c}
            onClick={() => onChange(c)}
          />
        ))}
      </div>
    </div>
  )
}

export function SettingsDialog({ initial, configPath, unread, onSave, onClose }: Props) {
  const [s, setS] = useState<Settings>(initial)
  const [tab, setTab] = useState('trigger')
  const [autostart, setAutostart] = useState(false)
  const [screenSize, setScreenSize] = useState<VirtualScreenSize>({ width: 1920, height: 1080 })

  useEffect(() => {
    void api.isAutostartEnabled().then(setAutostart)
    void api.virtualScreenSize().then(setScreenSize)
  }, [])

  useEffect(() => {
    if (tab !== 'hint') return
    void api.showGestureHintPreview({
      font_size: s.gesture_hint_font_size,
      color: s.gesture_hint_color,
      offset_x: s.gesture_hint_offset_x,
      offset_y: s.gesture_hint_offset_y,
    })
    return () => void api.hideGestureHintPreview()
  }, [
    tab,
    s.gesture_hint_font_size,
    s.gesture_hint_color,
    s.gesture_hint_offset_x,
    s.gesture_hint_offset_y,
  ])

  const toggleAutostart = (on: boolean) => {
    setAutostart(on)
    void api.setAutostartEnabled(on)
  }

  const toggleTrigger = (b: TriggerButton, on: boolean) =>
    setS({
      ...s,
      trigger_buttons: on
        ? [...TRIGGERS.filter((x) => x === b || s.trigger_buttons.includes(x))]
        : s.trigger_buttons.filter((x) => x !== b),
    })

  const xLimit = Math.floor(screenSize.width / 2)
  const yLimit = Math.floor(screenSize.height / 2)

  return (
    <div className="mask" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <div className="dialog">
        <div className="dialog-head">
          <h2>{t('settings')}</h2>
          <button type="button" className="btn-icon" onClick={onClose} title={t('cancel')}>
            <X size={16} strokeWidth={2} />
          </button>
        </div>

        <div className="dialog-body settings-body">
          <nav className="tabs">
            {TABS.map((x) => (
              <button
                key={x.id}
                type="button"
                className={tab === x.id ? 'tab on' : 'tab'}
                onClick={() => setTab(x.id)}
              >
                <x.icon size={16} strokeWidth={2} />
                {t(x.label)}
                {x.id === 'feedback' && unread > 0 && <span className="dot" />}
              </button>
            ))}
          </nav>

          <div className="tab-panel">
            {tab === 'trigger' && (
              <>
                <div className="field">
                  <label>{t('triggerButtons')}</label>
                  {TRIGGERS.map((b) => (
                    <label className="check" key={b} style={{ marginBottom: 6 }}>
                      <input
                        type="checkbox"
                        checked={s.trigger_buttons.includes(b)}
                        onChange={(e) => toggleTrigger(b, e.target.checked)}
                      />
                      {TRIGGER_LABEL[b]}
                    </label>
                  ))}
                </div>

                <div className="row field">
                  <div>
                    <label htmlFor="s-move">{t('moveThreshold')}</label>
                    <input
                      id="s-move"
                      type="number"
                      min={1}
                      max={200}
                      value={s.move_threshold}
                      onChange={(e) => setS({ ...s, move_threshold: Number(e.target.value) })}
                    />
                  </div>
                  <div>
                    <label htmlFor="s-match">{t('matchThreshold')}</label>
                    <input
                      id="s-match"
                      type="number"
                      min={0.5}
                      max={1}
                      step={0.01}
                      value={s.match_threshold}
                      onChange={(e) => setS({ ...s, match_threshold: Number(e.target.value) })}
                    />
                  </div>
                </div>

                <div className="field">
                  <label>{t('toggleHotkey')}</label>
                  <HotkeyRecorder
                    keys={s.toggle_hotkey}
                    onChange={(keys) => setS({ ...s, toggle_hotkey: keys })}
                  />
                </div>
              </>
            )}

            {tab === 'trail' && (
              <>
                <label className="check field">
                  <input
                    type="checkbox"
                    checked={s.show_trail}
                    onChange={(e) => setS({ ...s, show_trail: e.target.checked })}
                  />
                  {t('showTrail')}
                </label>

                <div className="field">
                  <label>{t('trailPreview')}</label>
                  <div className="trail-preview">
                    <StrokeCanvas
                      points={PREVIEW_STROKE}
                      color={s.trail_color}
                      width={s.trail_width}
                      glowColor={s.glow_color}
                      glowBlur={s.glow_blur}
                    />
                  </div>
                </div>

                <ColorField
                  id="s-color"
                  label={t('trailColor')}
                  value={s.trail_color}
                  onChange={(trail_color) => setS({ ...s, trail_color })}
                />

                <ColorField
                  id="s-miss"
                  label={t('missColor')}
                  value={s.miss_color}
                  onChange={(miss_color) => setS({ ...s, miss_color })}
                />

                <div className="field">
                  <label htmlFor="s-width">{t('trailWidth')}</label>
                  <input
                    id="s-width"
                    type="number"
                    min={1}
                    max={16}
                    step={0.5}
                    value={s.trail_width}
                    onChange={(e) => setS({ ...s, trail_width: Number(e.target.value) })}
                  />
                </div>

                <ColorField
                  id="s-glow"
                  label={t('glowColor')}
                  value={s.glow_color}
                  onChange={(glow_color) => setS({ ...s, glow_color })}
                />

                <div className="field">
                  <label htmlFor="s-glow-blur">{t('glowBlur')}</label>
                  <input
                    id="s-glow-blur"
                    type="number"
                    min={0}
                    max={40}
                    step={1}
                    value={s.glow_blur}
                    onChange={(e) => setS({ ...s, glow_blur: Number(e.target.value) })}
                  />
                </div>
              </>
            )}

            {tab === 'hint' && (
              <>
                <div className="field">
                  <label htmlFor="s-hint-size">{t('hintFontSize')}</label>
                  <div className="range-row">
                    <input
                      id="s-hint-size"
                      type="range"
                      min={12}
                      max={48}
                      step={1}
                      value={s.gesture_hint_font_size}
                      onChange={(e) => setS({ ...s, gesture_hint_font_size: Number(e.target.value) })}
                    />
                    <input
                      type="number"
                      min={12}
                      max={48}
                      value={s.gesture_hint_font_size}
                      onChange={(e) => setS({ ...s, gesture_hint_font_size: Number(e.target.value) })}
                    />
                  </div>
                </div>

                <ColorField
                  id="s-hint-color"
                  label={t('hintColor')}
                  value={s.gesture_hint_color}
                  onChange={(gesture_hint_color) => setS({ ...s, gesture_hint_color })}
                />

                <div className="field">
                  <label htmlFor="s-hint-x">{t('hintOffsetX')}</label>
                  <div className="range-row">
                    <input
                      id="s-hint-x"
                      type="range"
                      min={-xLimit}
                      max={xLimit}
                      step={1}
                      value={s.gesture_hint_offset_x}
                      onChange={(e) => setS({ ...s, gesture_hint_offset_x: Number(e.target.value) })}
                    />
                    <input
                      type="number"
                      min={-xLimit}
                      max={xLimit}
                      value={s.gesture_hint_offset_x}
                      onChange={(e) => setS({ ...s, gesture_hint_offset_x: Number(e.target.value) })}
                    />
                  </div>
                </div>

                <div className="field">
                  <label htmlFor="s-hint-y">{t('hintOffsetY')}</label>
                  <div className="range-row">
                    <input
                      id="s-hint-y"
                      type="range"
                      min={-yLimit}
                      max={yLimit}
                      step={1}
                      value={s.gesture_hint_offset_y}
                      onChange={(e) => setS({ ...s, gesture_hint_offset_y: Number(e.target.value) })}
                    />
                    <input
                      type="number"
                      min={-yLimit}
                      max={yLimit}
                      value={s.gesture_hint_offset_y}
                      onChange={(e) => setS({ ...s, gesture_hint_offset_y: Number(e.target.value) })}
                    />
                  </div>
                </div>
                <p className="field-note">
                  {t('hintScreenSize')} {screenSize.width} x {screenSize.height}px. {t('hintOffsetHelp')}
                </p>
              </>
            )}

            {tab === 'feedback' && (
              <FeedbackPane
                nickname={s.feedback_nickname}
                onNickname={(feedback_nickname) => setS({ ...s, feedback_nickname })}
              />
            )}

            {tab === 'about' && (
              <>
                <label className="check field">
                  <input
                    type="checkbox"
                    checked={autostart}
                    onChange={(e) => toggleAutostart(e.target.checked)}
                  />
                  {t('autostart')}
                </label>

                <div className="field">
                  <label>{t('configPath')}</label>
                  <p className="hint-path">{configPath}</p>
                </div>
              </>
            )}
          </div>
        </div>

        <div className="dialog-foot">
          <button type="button" className="btn-outline subtle" onClick={onClose}>
            {t('cancel')}
          </button>
          <button type="button" className="btn-primary" onClick={() => onSave(s)}>
            <Check size={16} strokeWidth={2} />
            {t('save')}
          </button>
        </div>
      </div>
    </div>
  )
}
