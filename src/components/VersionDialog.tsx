import { useEffect, useRef, useState } from 'react'
import type { Update } from '@tauri-apps/plugin-updater'
import { relaunch } from '@tauri-apps/plugin-process'
import { Download } from 'lucide-react'
import { t } from '../i18n'
import bundledNotes from '../../Release.md?raw'

interface Props {
  version: string
  update: Update | null
  updateStatus: 'checking' | 'checked' | 'failed'
  onCheck: () => void
  onClose: () => void
}

function ReleaseNotes({ body }: { body: string }) {
  return (
    <div className="release-notes">
      {body.trim().split(/\r?\n\s*\n/).map((block, index) => {
        if (/^#{1,6}\s/.test(block) && !block.includes('\n')) {
          return <h4 key={index}>{block.replace(/^#{1,6}\s+/, '')}</h4>
        }
        const lines = block.split(/\r?\n/)
        if (lines.every((line) => /^[-*]\s/.test(line))) {
          return <ul key={index}>{lines.map((line, i) => <li key={i}>{line.slice(2)}</li>)}</ul>
        }
        return <p key={index}>{block}</p>
      })}
    </div>
  )
}

export function VersionDialog({ version, update, updateStatus, onCheck, onClose }: Props) {
  const [installing, setInstalling] = useState(false)
  const [error, setError] = useState('')
  const dialog = useRef<HTMLDialogElement>(null)
  const headingVersion = bundledNotes.trimStart().match(/^#{1,6}\s+v(\S+)/)?.[1]
  const currentNotes = headingVersion === version ? bundledNotes : ''
  const availableUpdate = updateStatus === 'checked' ? update : null

  useEffect(() => {
    const element = dialog.current
    element?.showModal()
    return () => element?.close()
  }, [])

  const dismiss = () => {
    if (installing) return
    dialog.current?.close()
    onClose()
  }

  const install = async () => {
    if (!availableUpdate || installing) return
    setInstalling(true)
    setError('')
    try {
      await availableUpdate.downloadAndInstall()
      await relaunch()
    } catch (e) {
      setInstalling(false)
      setError(t('updateFailed') + String(e))
    }
  }

  return (
    <dialog
      ref={dialog}
      className="dialog version-dialog"
      aria-labelledby="version-dialog-title"
      onCancel={(event) => {
        event.preventDefault()
        dismiss()
      }}
    >
      <div className="dialog-head">
        <h2 id="version-dialog-title">{t('updateCurrent')} v{version}</h2>
      </div>
      <div className="dialog-body">
        <section aria-label={t('versionNotes')}>
          {currentNotes ? <ReleaseNotes body={currentNotes} /> : <p>{t('versionNotesMissing')}</p>}
        </section>
        <section className="version-update" aria-label={t('updateSection')}>
          <p role="status">
            {updateStatus === 'checking' ? t('updateChecking') :
              updateStatus === 'failed' ? t('updateCheckFailed') :
                availableUpdate ? <strong>{t('updateAvailable')} v{availableUpdate.version}</strong> :
                  t('updateUpToDate')}
          </p>
          {availableUpdate?.body && <ReleaseNotes body={availableUpdate.body} />}
        </section>
      </div>
      <div className="dialog-foot">
        {error && <span className="error" role="alert" style={{ marginRight: 'auto' }}>{error}</span>}
        <button type="button" className="btn-outline subtle" disabled={installing} onClick={dismiss}>
          {t('close')}
        </button>
        {updateStatus === 'failed' && (
          <button type="button" className="btn-primary" onClick={onCheck}>{t('updateRetry')}</button>
        )}
        {availableUpdate && (
          <button type="button" className="btn-primary" disabled={installing} onClick={() => void install()}>
            <Download size={16} strokeWidth={2} />
            {installing ? t('updateDownloading') : t('updateNow')}
          </button>
        )}
      </div>
    </dialog>
  )
}
