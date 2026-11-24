import { useEffect, useRef, useState } from 'react'
import { listen, type UnlistenFn } from '@tauri-apps/api/event'
import { Keyboard, Play, Square, X } from 'lucide-react'
import { api, newId, type Action, type KeySequenceEvent } from '../api'
import { t } from '../i18n'

type SequenceAction = Extract<Action, { kind: 'key_sequence' }>
type Phase = 'idle' | 'starting' | 'recording' | 'stopping'

interface Props {
  action: SequenceAction
  onChange: (action: SequenceAction) => void
  onError: (message: string) => void
  onRecordingChange: (recording: boolean) => void
}

export function KeySequenceRecorder({ action, onChange, onError, onRecordingChange }: Props) {
  const [phase, setPhase] = useState<Phase>('idle')
  const [live, setLive] = useState<KeySequenceEvent[]>([])
  const [testSeconds, setTestSeconds] = useState<number | null>(null)
  const session = useRef<{ id: string; unlisten?: UnlistenFn } | null>(null)
  const list = useRef<HTMLOListElement>(null)
  const busy = phase !== 'idle'
  const hasSequence = !!(action.events?.length || action.steps?.length)

  useEffect(() => {
    return () => {
      const current = session.current
      session.current = null
      current?.unlisten?.()
      if (current) void api.cancelKeySequence(current.id).catch(() => {})
      onRecordingChange(false)
    }
  }, [onRecordingChange])

  useEffect(() => {
    if (phase === 'idle' || phase === 'stopping') return
    const cancel = () => {
      const current = session.current
      if (!current) return
      session.current = null
      current.unlisten?.()
      void api.cancelKeySequence(current.id).catch(() => {})
      setPhase('idle')
      onRecordingChange(false)
      onError(t('sequenceInterrupted'))
    }
    window.addEventListener('blur', cancel)
    if (!document.hasFocus()) cancel()
    return () => window.removeEventListener('blur', cancel)
  }, [phase, onError, onRecordingChange])

  useEffect(() => {
    if (list.current) {
      if (phase === 'recording') list.current.scrollTop = list.current.scrollHeight
      if (phase === 'idle') list.current.scrollTop = 0
    }
  }, [live, phase])

  useEffect(() => {
    if (testSeconds === null) return
    if (testSeconds > 0) {
      const timer = window.setTimeout(() => setTestSeconds(testSeconds - 1), 1000)
      return () => window.clearTimeout(timer)
    }
    let active = true
    void api.testAction(action).catch((error) => {
      if (active) onError(String(error))
    }).finally(() => {
      if (active) setTestSeconds(null)
    })
    return () => { active = false }
  }, [testSeconds, action, onError])

  const start = async () => {
    const current: NonNullable<typeof session.current> = { id: newId() }
    session.current = current
    onError('')
    onRecordingChange(true)
    setLive([])
    setPhase('starting')
    try {
      current.unlisten = await listen<{ id: string; event: KeySequenceEvent }>('key-sequence:event', ({ payload }) => {
        if (session.current === current && payload.id === current.id) {
          setLive((events) => [...events, payload.event])
        }
      })
      if (session.current !== current) {
        current.unlisten()
        return
      }
      await api.beginKeySequence(current.id)
      if (session.current !== current) {
        await api.cancelKeySequence(current.id)
        return
      }
      setPhase('recording')
    } catch (error) {
      if (session.current !== current) return
      current.unlisten?.()
      session.current = null
      setPhase('idle')
      onRecordingChange(false)
      onError(String(error))
    }
  }

  const stop = async () => {
    const current = session.current
    if (!current) return
    setPhase('stopping')
    try {
      const events = await api.stopKeySequence(current.id)
      if (session.current !== current) return
      if (events.length) onChange({ kind: 'key_sequence', events })
      else onError(t('sequenceEmpty'))
    } catch (error) {
      if (session.current === current) onError(String(error))
    } finally {
      if (session.current === current) {
        current.unlisten?.()
        session.current = null
        setPhase('idle')
        onRecordingChange(false)
      }
    }
  }

  const events = busy ? live : action.events ?? []
  const legacy = !busy && events.length === 0 ? action.steps ?? [] : []

  return (
    <div className="field key-sequence">
      <div className={busy ? 'hotkey-box sequence-box live' : 'hotkey-box sequence-box'}>
        {events.length > 0 || legacy.length > 0 ? (
          <ol className="sequence-list" ref={list} aria-label={t('actionKeySequence')}>
            {events.map((event, index) => (
              <li key={index}>
                <span className="tag">{event.label}</span>
                <span>{t(event.down ? 'sequenceDown' : 'sequenceUp')}</span>
                {index > 0 && <span className="sequence-delay">{t('sequenceDelay')} {event.delay_ms} ms</span>}
              </li>
            ))}
            {legacy.map((step, index) => (
              <li key={index}>
                <span className="tag">{step.join('+')}</span>
                {index > 0 && <span className="sequence-delay">{t('sequenceDelay')} 30 ms</span>}
              </li>
            ))}
          </ol>
        ) : (
          <span className="hotkey-hint">{busy ? t('sequenceHint') : t('noKeys')}</span>
        )}
      </div>
      <div className="sequence-buttons">
        <button
          type="button"
          className="btn-outline subtle"
          disabled={phase === 'starting' || phase === 'stopping' || testSeconds !== null}
          onClick={phase === 'recording' ? stop : start}
        >
          {busy ? <Square size={16} /> : <Keyboard size={16} />}
          {phase === 'starting' ? t('startingSequence') : phase === 'stopping' ? t('stoppingSequence')
            : phase === 'recording' ? t('stopSequence') : hasSequence ? t('reRecord') : t('recordSequence')}
        </button>
        <button type="button" className="btn-outline subtle" disabled={busy || !hasSequence || testSeconds !== null}
          onClick={() => { onError(''); setTestSeconds(3) }}>
          <Play size={16} />
          {testSeconds ? `${t('test')} (${testSeconds})` : t('test')}
        </button>
        {hasSequence && !busy && testSeconds === null && (
          <button type="button" className="btn-icon" title={t('clearKeys')} aria-label={t('clearKeys')}
            onClick={() => { onError(''); onChange({ kind: 'key_sequence' }) }}>
            <X size={16} />
          </button>
        )}
      </div>
      {!!testSeconds && <p className="field-note" role="status">{t('sequenceTestHint')}</p>}
    </div>
  )
}
