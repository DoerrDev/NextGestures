import { useEffect, useId, useLayoutEffect, useRef, useState } from 'react'
import { AppWindow, Check, X } from 'lucide-react'
import { api, type InstalledApp } from '../api'
import { t } from '../i18n'

interface Props {
  selected?: Pick<InstalledApp, 'id' | 'name'>
  onSelect: (app: InstalledApp) => void
}

type Catalog =
  | { status: 'loading' }
  | { status: 'ready'; apps: InstalledApp[] }
  | { status: 'error' }

export function ProgramPicker({ selected, onSelect }: Props) {
  const [open, setOpen] = useState(false)
  const buttonRef = useRef<HTMLButtonElement>(null)
  const [picked, setPicked] = useState<InstalledApp>()
  const close = () => {
    setOpen(false)
    requestAnimationFrame(() => buttonRef.current?.focus())
  }

  return (
    <div className="field">
      {selected && (
        <div className="program-selection">
          <span className="program-icon" aria-hidden="true">
            {picked?.id === selected.id && picked.icon ? <img src={picked.icon} alt="" /> : <AppWindow />}
          </span>
          <span>
            <span className="program-selection-label">{t('program')}</span>
            <strong>{selected.name}</strong>
          </span>
        </div>
      )}
      <button type="button" className="btn-outline subtle" ref={buttonRef} aria-haspopup="dialog" onClick={() => setOpen(true)}>
        <AppWindow size={16} />
        {t(selected ? 'changeInstalledApp' : 'chooseInstalledApp')}
      </button>
      {open && (
        <ProgramDialog
          selectedId={selected?.id}
          onDismiss={close}
          onConfirm={(app) => {
            setPicked(app)
            onSelect(app)
            close()
          }}
        />
      )}
    </div>
  )
}

function ProgramDialog({ selectedId, onDismiss, onConfirm }: {
  selectedId?: string
  onDismiss: () => void
  onConfirm: (app: InstalledApp) => void
}) {
  const dialogRef = useRef<HTMLDialogElement>(null)
  const searchRef = useRef<HTMLInputElement>(null)
  const listRef = useRef<HTMLUListElement>(null)
  const titleId = useId()
  const [query, setQuery] = useState('')
  const [candidateId, setCandidateId] = useState(selectedId)
  const [catalog, setCatalog] = useState<Catalog>({ status: 'loading' })
  const [attempt, setAttempt] = useState(0)

  useLayoutEffect(() => {
    const dialog = dialogRef.current
    if (dialog && !dialog.open) dialog.showModal()
    searchRef.current?.focus()
  }, [])

  useEffect(() => {
    let active = true
    void api.listInstalledApps().then(
      (apps) => {
        if (!active) return
        apps.sort((a, b) => a.name.localeCompare(b.name, 'zh-CN', { numeric: true }))
        setCatalog({ status: 'ready', apps })
      },
      () => { if (active) setCatalog({ status: 'error' }) },
    )
    return () => { active = false }
  }, [attempt])

  const search = query.trim().toLowerCase()
  const matches = catalog.status === 'ready'
    ? catalog.apps.filter((app) => app.name.toLowerCase().includes(search) || app.path.toLowerCase().includes(search))
    : []
  const candidate = matches.find((app) => app.id === candidateId)

  return (
    <dialog
      ref={dialogRef}
      className="dialog program-dialog"
      aria-labelledby={titleId}
      onCancel={(event) => {
        event.preventDefault()
        onDismiss()
      }}
    >
      <header className="dialog-head">
        <h2 id={titleId}>{t('chooseInstalledApp')}</h2>
        <button type="button" className="btn-icon" aria-label={t('cancel')} onClick={onDismiss}><X size={18} /></button>
      </header>
      <div className="dialog-body program-dialog-body">
        <input
          ref={searchRef}
          type="text"
          aria-label={t('searchInstalledApps')}
          placeholder={t('searchInstalledApps')}
          value={query}
          onChange={(event) => {
            setQuery(event.target.value)
            if (listRef.current) listRef.current.scrollTop = 0
          }}
        />
        {catalog.status === 'loading' && <p className="field-note" role="status">{t('loadingInstalledApps')}</p>}
        {catalog.status === 'error' && (
          <div>
            <p className="error" role="alert">{t('installedAppsFailed')}</p>
            <button type="button" className="btn-ghost" onClick={() => {
              setCatalog({ status: 'loading' })
              setAttempt((value) => value + 1)
            }}>{t('retryInstalledApps')}</button>
          </div>
        )}
        {catalog.status === 'ready' && (matches.length ? (
          <ul ref={listRef} className="program-picker-list" aria-label={t('chooseInstalledApp')}>
            {matches.map((app) => (
              <li key={app.id}>
                <button
                  type="button"
                  className="program-row"
                  aria-pressed={candidateId === app.id}
                  onClick={() => setCandidateId(app.id)}
                  onDoubleClick={() => onConfirm(app)}
                >
                  <span className="program-icon" aria-hidden="true">
                    {app.icon ? <img src={app.icon} alt="" loading="lazy" /> : <AppWindow />}
                  </span>
                  <span className="program-row-details">
                    <strong>{app.name}</strong>
                    <span className="program-path" title={app.path}>{app.path}</span>
                  </span>
                  {candidateId === app.id && <Check size={18} aria-hidden="true" />}
                </button>
              </li>
            ))}
          </ul>
        ) : <p className="field-note" role="status">{t('noInstalledApps')}</p>)}
      </div>
      <footer className="dialog-foot">
        <button type="button" className="btn-ghost" onClick={onDismiss}>{t('cancel')}</button>
        <button type="button" className="btn-primary" disabled={!candidate} onClick={() => {
          if (candidate) onConfirm(candidate)
        }}>{t('confirm')}</button>
      </footer>
    </dialog>
  )
}
