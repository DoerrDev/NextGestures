import { useEffect, useId, useRef, useState } from 'react'
import { AppWindow } from 'lucide-react'
import { api, type InstalledApp } from '../api'
import { t } from '../i18n'

interface Props {
  selected?: string
  onSelect: (app: InstalledApp) => void
  onManual: () => void
}

type Catalog =
  | { status: 'loading' }
  | { status: 'ready'; apps: InstalledApp[] }
  | { status: 'error' }

export function ProgramPicker({ selected, onSelect, onManual }: Props) {
  const [open, setOpen] = useState(false)
  const panelId = useId()
  const buttonRef = useRef<HTMLButtonElement>(null)
  const close = () => {
    setOpen(false)
    buttonRef.current?.focus()
  }

  return (
    <div className="field">
      {selected && <label>{t('program')}</label>}
      {selected && <p className="program-selection">{selected}</p>}
      <div className="program-picker-buttons">
        <button
          type="button"
          className="btn-outline subtle"
          ref={buttonRef}
          aria-expanded={open}
          aria-controls={open ? panelId : undefined}
          onClick={() => setOpen(!open)}
        >
          <AppWindow size={16} />
          {t(selected ? 'changeInstalledApp' : 'chooseInstalledApp')}
        </button>
        {selected && (
          <button type="button" className="btn-ghost" onClick={() => { close(); onManual() }}>
            {t('manualCommand')}
          </button>
        )}
      </div>
      {open && (
        <ProgramCatalog
          id={panelId}
          onClose={close}
          onSelect={(app) => {
            onSelect(app)
            close()
          }}
        />
      )}
    </div>
  )
}

function ProgramCatalog({ id, onSelect, onClose }: {
  id: string
  onSelect: (app: InstalledApp) => void
  onClose: () => void
}) {
  const [query, setQuery] = useState('')
  const [catalog, setCatalog] = useState<Catalog>({ status: 'loading' })
  const [attempt, setAttempt] = useState(0)
  const panelRef = useRef<HTMLDivElement>(null)

  useEffect(() => {
    let active = true
    void api.listInstalledApps().then(
      (apps) => {
        if (active) {
          apps.sort((a, b) => a.name.localeCompare(b.name, 'zh-CN', { numeric: true }))
          setCatalog({ status: 'ready', apps })
        }
      },
      () => { if (active) setCatalog({ status: 'error' }) },
    )
    return () => { active = false }
  }, [attempt])

  useEffect(() => {
    if (catalog.status !== 'loading') panelRef.current?.scrollIntoView({ block: 'nearest' })
  }, [catalog.status])

  const search = query.trim().toLowerCase()
  const matches = catalog.status === 'ready'
    ? catalog.apps.filter((app) => app.name.toLowerCase().includes(search))
    : []

  return (
    <div id={id} ref={panelRef} className="program-picker-panel" onKeyDown={(e) => {
      if (e.key === 'Escape' && !e.nativeEvent.isComposing) {
        e.stopPropagation()
        onClose()
      }
    }}>
      <input
        type="text"
        autoFocus
        aria-label={t('searchInstalledApps')}
        placeholder={t('searchInstalledApps')}
        value={query}
        onChange={(e) => setQuery(e.target.value)}
      />
      {catalog.status === 'loading' && <p className="field-note" role="status">{t('loadingInstalledApps')}</p>}
      {catalog.status === 'error' && (
        <div className="program-picker-buttons">
          <p className="error" role="alert">{t('installedAppsFailed')}</p>
          <button type="button" className="btn-ghost" onClick={() => {
            setCatalog({ status: 'loading' })
            setAttempt((value) => value + 1)
          }}>
            {t('retryInstalledApps')}
          </button>
        </div>
      )}
      {catalog.status === 'ready' && (matches.length ? (
        <ul className="program-picker-list" aria-label={t('chooseInstalledApp')}>
          {matches.map((app) => (
            <li key={app.id}>
              <button type="button" className="btn-ghost" onClick={() => onSelect(app)}>{app.name}</button>
            </li>
          ))}
        </ul>
      ) : <p className="field-note" role="status">{t('noInstalledApps')}</p>)}
    </div>
  )
}
