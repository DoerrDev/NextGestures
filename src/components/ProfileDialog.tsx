import { useEffect, useState } from 'react'
import { listen } from '@tauri-apps/api/event'
import { Check, Crosshair, X } from 'lucide-react'
import { api, newId, type Profile } from '../api'
import { t } from '../i18n'

interface Props {
  onSave: (profile: Profile) => void
  onClose: () => void
}

interface PickInfo {
  exe: string
  path: string
  title: string
  icon: string
}

/** 窗口标题通常形如「文档名 - 项目名 - 应用名」，取最后一段作为显示名称 */
function appNameOf(info: PickInfo) {
  const tail = info.title.split(/\s+[-–—|]\s+/).pop()?.trim()
  return tail || info.exe.replace(/\.exe$/i, '')
}

export function ProfileDialog({ onSave, onClose }: Props) {
  const [name, setName] = useState('')
  const [exe, setExe] = useState('')
  const [picking, setPicking] = useState(false)
  const [preview, setPreview] = useState<PickInfo | null>(null)
  const [error, setError] = useState('')

  useEffect(() => {
    const offHover = listen<PickInfo | null>('pick:hover', (e) => {
      setPreview(e.payload)
    })
    const offResult = listen<PickInfo | null>('pick:result', (e) => {
      setPicking(false)
      if (!e.payload) {
        setPreview(null)
        return
      }
      const info = e.payload
      setExe(info.exe)
      setName(appNameOf(info))
      setPreview(info)
    })
    return () => {
      void offHover.then((f) => f())
      void offResult.then((f) => f())
    }
  }, [])

  const pick = async () => {
    setPicking(true)
    await api.beginPick()
  }

  const submit = () => {
    const target = exe.trim().toLowerCase()
    if (!target) return setError(t('needExe'))
    onSave({ id: newId(), name: name.trim() || target, exe: target, gestures: [] })
  }

  return (
    <div className="mask" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <div className="dialog narrow">
        <div className="dialog-head">
          <h2>{t('addProfile')}</h2>
          <button type="button" className="btn-icon" onClick={onClose} title={t('cancel')}>
            <X size={16} strokeWidth={2} />
          </button>
        </div>
        <div className="dialog-body">
          <div className="field">
            <label htmlFor="p-exe">{t('exeName')}</label>
            <input
              id="p-exe"
              type="text"
              value={exe}
              placeholder={t('exePlaceholder')}
              onChange={(e) => setExe(e.target.value)}
            />
          </div>
          <button
            type="button"
            className="btn-outline subtle"
            onMouseDown={pick}
            disabled={picking}
          >
            <Crosshair size={16} strokeWidth={2} />
            {picking ? t('picking') : t('pick')}
          </button>
          {preview && (
            <div className="pick-preview">
              {preview.icon && <img src={preview.icon} alt="" />}
              <div className="pick-preview-text">
                <div className="pick-preview-title">{preview.title || preview.exe}</div>
                <div className="pick-preview-path">{preview.path || preview.exe}</div>
              </div>
            </div>
          )}
          <div className="field">
            <label htmlFor="p-name">{t('displayName')}</label>
            <input
              id="p-name"
              type="text"
              value={name}
              onChange={(e) => setName(e.target.value)}
            />
          </div>
        </div>
        <div className="dialog-foot">
          {error && <span className="error" style={{ marginRight: 'auto' }}>{error}</span>}
          <button type="button" className="btn-outline subtle" onClick={onClose}>
            {t('cancel')}
          </button>
          <button type="button" className="btn-primary" onClick={submit}>
            <Check size={16} strokeWidth={2} />
            {t('save')}
          </button>
        </div>
      </div>
    </div>
  )
}
