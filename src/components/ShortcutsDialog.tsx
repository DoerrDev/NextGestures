import { useState } from 'react'
import { Keyboard, Plus, X } from 'lucide-react'
import { type Shortcut } from '../api'
import { t } from '../i18n'
import { ActionEditor, describeAction } from './ActionEditor'
import { HotkeyRecorder } from './HotkeyRecorder'

interface Props {
  shortcuts: Shortcut[]
  onSave: (shortcuts: Shortcut[]) => Promise<void>
  onClose: () => void
}

export function ShortcutsDialog({ shortcuts, onSave, onClose }: Props) {
  const [draft, setDraft] = useState<{ index: number; shortcut: Shortcut } | null>(null)
  const [error, setError] = useState('')
  const [saving, setSaving] = useState(false)
  const [recordingAction, setRecordingAction] = useState(false)
  const [recordingTrigger, setRecordingTrigger] = useState(false)
  const recording = recordingAction || recordingTrigger

  const edit = (index: number, shortcut: Shortcut) => {
    setError('')
    setDraft({ index, shortcut })
  }

  const save = async (next: Shortcut[]) => {
    setError('')
    setSaving(true)
    try {
      await onSave(next)
      setDraft(null)
    } catch (e) {
      setError(String(e))
    } finally {
      setSaving(false)
    }
  }

  const saveDraft = () => {
    if (!draft) return
    const next = shortcuts.slice()
    next[draft.index] = draft.shortcut
    void save(next)
  }

  return (
    <div className="mask" onMouseDown={(e) => {
      if (e.target === e.currentTarget && !saving && !recording) onClose()
    }}>
      <div className="dialog" role="dialog" aria-modal="true" aria-labelledby="shortcuts-title">
        <div className="dialog-head">
          <h2 id="shortcuts-title"><Keyboard size={18} /> {t('shortcuts')}</h2>
          <button type="button" className="btn-icon" onClick={onClose} disabled={saving || recording} title={t('close')}>
            <X size={16} />
          </button>
        </div>
        <div className="dialog-body shortcut-body">
          <p>{t('shortcutHint')}</p>
          {draft ? (
            <fieldset className="shortcut-editor" disabled={saving}>
              <fieldset className="shortcut-field field" disabled={recordingAction}>
                <span>{t('shortcutTrigger')}</span>
                <HotkeyRecorder keys={draft.shortcut.keys} onRecordingChange={setRecordingTrigger} onChange={(keys) =>
                  setDraft({ ...draft, shortcut: { ...draft.shortcut, keys } })} />
              </fieldset>
              <fieldset className="shortcut-field" disabled={recordingTrigger}>
                <ActionEditor
                  action={draft.shortcut.action}
                  onChange={(action) => setDraft({ ...draft, shortcut: { ...draft.shortcut, action } })}
                  onError={setError}
                  onRecordingChange={setRecordingAction}
                />
              </fieldset>
            </fieldset>
          ) : shortcuts.length === 0 ? <p>{t('shortcutEmpty')}</p> : (
            <ul className="shortcut-list">
              {shortcuts.map((shortcut, index) => (
                <li key={index}>
                  <span className="tag">{shortcut.keys.join(' + ')}</span>
                  <span>{describeAction(shortcut.action)}</span>
                  <button type="button" className="btn-outline subtle" disabled={saving}
                    onClick={() => edit(index, shortcut)}>{t('edit')}</button>
                  <button type="button" className="btn-outline subtle" disabled={saving}
                    onClick={() => void save(shortcuts.filter((_, i) => i !== index))}>{t('remove')}</button>
                </li>
              ))}
            </ul>
          )}
          {error && <p className="shortcut-error" role="alert">{error}</p>}
        </div>
        <div className="dialog-foot">
          {draft ? (
            <>
              <button type="button" className="btn-outline subtle" disabled={saving || recording}
                onClick={() => { setDraft(null); setError('') }}>{t('cancel')}</button>
              <button type="button" className="btn-primary" onClick={saveDraft}
                disabled={saving || recording || draft.shortcut.keys.length < 2 || draft.shortcut.action.kind === 'none'}>
                {t('save')}
              </button>
            </>
          ) : (
            <button type="button" className="btn-primary" disabled={saving}
              onClick={() => edit(shortcuts.length, { keys: [], action: { kind: 'none' } })}>
              <Plus size={16} /> {t('shortcutAdd')}
            </button>
          )}
        </div>
      </div>
    </div>
  )
}
