import { useEffect, useRef, useState } from 'react'
import { listen } from '@tauri-apps/api/event'
import { Crosshair, Plus, Trash2 } from 'lucide-react'
import { api } from '../api'
import { t } from '../i18n'

interface PickInfo {
  exe: string
  path: string
  title: string
  icon: string
}

export function BlacklistPane({ names, onChange }: {
  names: string[]
  onChange: (names: string[]) => Promise<void>
}) {
  const [exe, setExe] = useState('')
  const [error, setError] = useState('')
  const [message, setMessage] = useState('')
  const [picking, setPicking] = useState(false)
  const [saving, setSaving] = useState(false)
  const [ready, setReady] = useState(false)
  const [preview, setPreview] = useState<PickInfo | null>(null)
  const active = useRef(false)
  const busy = useRef(false)
  const addRef = useRef<(value: string) => Promise<void>>(async () => {})

  useEffect(() => {
    let disposed = false
    const listeners = [
      listen<PickInfo | null>('pick:hover', ({ payload }) => {
        if (active.current) setPreview(payload)
      }),
      listen<PickInfo | null>('pick:result', ({ payload }) => {
        if (!active.current) return
        active.current = false
        setPicking(false)
        setPreview(payload)
        if (payload) void addRef.current(payload.exe)
        else setError(t('blacklistPickEmpty'))
      }),
    ]
    void Promise.all(listeners).then(() => {
      if (!disposed) setReady(true)
    }).catch((e) => { if (!disposed) setError(String(e)) })
    return () => {
      disposed = true
      if (active.current) {
        active.current = false
        void api.cancelPick()
      }
      for (const listener of listeners) void listener.then((off) => off()).catch(() => {})
    }
  }, [])

  const persist = async (next: string[], success: string) => {
    if (busy.current) return false
    busy.current = true
    setSaving(true)
    setMessage('')
    setError('')
    try {
      await onChange(next)
      setMessage(success)
      return true
    } catch (e) {
      setError(String(e))
      return false
    } finally {
      busy.current = false
      setSaving(false)
    }
  }

  const add = async (value: string) => {
    if (busy.current) return
    const name = value.trim().toLowerCase()
    setMessage('')
    if (!/^[^\\/:*?"<>|\x00-\x1f]+\.exe$/i.test(name)) return setError(t('blacklistInvalid'))
    if (names.some((x) => x.trim().toLowerCase() === name)) return setError(t('blacklistDuplicate'))
    if (await persist([...names, name], `${name} ${t('blacklistAdded')}`)) setExe('')
  }
  addRef.current = add

  const pick = async () => {
    active.current = true
    setPicking(true)
    setPreview(null)
    setError('')
    setMessage('')
    try {
      await api.beginPick()
    } catch (e) {
      active.current = false
      setPicking(false)
      setError(String(e))
    }
  }

  return (
    <>
      <p className="blacklist-help">{t('blacklistHelp')}</p>
      <div className="blacklist-pick">
        <button
          type="button"
          className="btn-primary"
          disabled={!ready || picking || saving}
          onMouseDown={(e) => { if (e.button === 0) void pick() }}
          onClick={(e) => { if (e.detail === 0) void pick() }}
        >
          <Crosshair size={16} />{picking ? t('picking') : t('blacklistPick')}
        </button>
      </div>
      {picking && <p role="status" className="blacklist-help">{t('blacklistPickHelp')}</p>}
      <div className="field">
        <label htmlFor="blacklist-exe">{t('exeName')}</label>
        <div className="blacklist-input">
          <input
            id="blacklist-exe"
            value={exe}
            placeholder="chrome.exe"
            disabled={picking || saving}
            onChange={(e) => { setExe(e.target.value); setError('') }}
            onKeyDown={(e) => { if (e.key === 'Enter') { e.preventDefault(); void add(exe) } }}
          />
          <button type="button" className="btn-outline subtle" disabled={picking || saving} onClick={() => void add(exe)}>
            <Plus size={16} />{t('blacklistAdd')}
          </button>
        </div>
      </div>
      {picking && preview && <div className="pick-preview">
        {preview.icon && <img src={preview.icon} alt="" />}
        <div className="pick-preview-text">
          <div className="pick-preview-title">{preview.title || preview.exe}</div>
          <div className="pick-preview-path">{preview.path || preview.exe}</div>
        </div>
      </div>}
      {error && <p className="error" role="alert">{error}</p>}
      {(saving || message) && <p className="blacklist-status" role="status">{saving ? t('blacklistSaving') : message}</p>}
      {names.length === 0 ? <p className="blacklist-help">{t('blacklistEmpty')}</p> : (
        <ul className="blacklist-list" aria-label={t('tabBlacklist')}>
          {names.map((name, index) => <li key={`${name}-${index}`}>
            <span>{name}</span>
            <button
              type="button" className="btn-icon" disabled={picking || saving}
              title={`${t('blacklistRemove')} ${name}`} aria-label={`${t('blacklistRemove')} ${name}`}
              onClick={() => void persist(names.filter((_, i) => i !== index), `${name} ${t('blacklistRemoved')}`)}
            ><Trash2 size={16} /></button>
          </li>)}
        </ul>
      )}
      <p className="blacklist-help">{t('blacklistSaveHint')}</p>
    </>
  )
}
