import { useState } from 'react'
import type { Update } from '@tauri-apps/plugin-updater'
import { relaunch } from '@tauri-apps/plugin-process'
import { Download, Sparkles } from 'lucide-react'
import { t } from '../i18n'

interface Props {
  update: Update
  onClose: () => void
}

export function UpdateDialog({ update, onClose }: Props) {
  const [installing, setInstalling] = useState(false)
  const [error, setError] = useState('')

  const install = async () => {
    setInstalling(true)
    setError('')
    try {
      await update.downloadAndInstall()
      await relaunch()
    } catch (e) {
      setInstalling(false)
      setError(t('updateFailed') + String(e))
    }
  }

  return (
    <div className="mask" onMouseDown={(e) => e.target === e.currentTarget && !installing && onClose()}>
      <div className="dialog narrow">
        <div className="dialog-head">
          <h2>
            <Sparkles size={16} strokeWidth={2} />
            {t('updateAvailable')} v{update.version}
          </h2>
        </div>
        <div className="dialog-body">
          <p className="subtitle">
            {t('updateCurrent')} v{update.currentVersion}
          </p>
          {update.body && <p style={{ whiteSpace: 'pre-wrap' }}>{update.body}</p>}
        </div>
        <div className="dialog-foot">
          {error && <span className="error" style={{ marginRight: 'auto' }}>{error}</span>}
          <button type="button" className="btn-outline subtle" disabled={installing} onClick={onClose}>
            {t('updateLater')}
          </button>
          <button type="button" className="btn-primary" disabled={installing} onClick={() => void install()}>
            <Download size={16} strokeWidth={2} />
            {installing ? t('updateDownloading') : t('updateNow')}
          </button>
        </div>
      </div>
    </div>
  )
}
