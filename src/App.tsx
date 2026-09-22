import { useCallback, useEffect, useMemo, useState } from 'react'
import { listen } from '@tauri-apps/api/event'
import { check, type Update } from '@tauri-apps/plugin-updater'
import {
  AppWindow,
  Globe,
  LayoutGrid,
  List,
  MoreHorizontal,
  Pencil,
  Plus,
  Power,
  PowerOff,
  Search,
  Settings2,
  Spline,
  Trash2,
  TriangleAlert,
  Zap,
} from 'lucide-react'
import {
  api,
  emptyGesture,
  type Config,
  type Gesture,
  type Profile,
  type Settings,
} from './api'
import { findConflicts } from './conflicts'
import { setLang, t } from './i18n'
import { describeAction } from './components/ActionEditor'
import { GesturePane } from './components/GesturePane'
import { ProfileDialog } from './components/ProfileDialog'
import { SettingsDialog } from './components/SettingsDialog'
import { StrokeCanvas } from './components/StrokeCanvas'
import { UpdateDialog } from './components/UpdateDialog'

const TRIGGER_LABEL: Record<string, string> = {
  Right: '右键',
  Middle: '中键',
  X1: '侧键 1',
  X2: '侧键 2',
}

type Filter = 'all' | 'on' | 'off'
type View = 'list' | 'card'

export function App() {
  const [config, setConfig] = useState<Config | null>(null)
  const [configPath, setConfigPath] = useState('')
  const [activeId, setActiveId] = useState('global')
  const [editing, setEditing] = useState<{ gesture: Gesture; isNew: boolean } | null>(null)
  const [profileOpen, setProfileOpen] = useState(false)
  const [settingsOpen, setSettingsOpen] = useState(false)
  const [unread, setUnread] = useState(0)
  const [enabled, setEnabled] = useState(true)
  const [toast, setToast] = useState('')
  const [confirmRemove, setConfirmRemove] = useState<Profile | null>(null)
  const [update, setUpdate] = useState<Update | null>(null)
  const [query, setQuery] = useState('')
  const [filter, setFilter] = useState<Filter>('all')
  const [menuId, setMenuId] = useState('')
  const [view, setView] = useState<View>(
    () => (localStorage.getItem('view') === 'card' ? 'card' : 'list'),
  )

  const [paneW, setPaneW] = useState(() => Number(localStorage.getItem('paneW')) || 400)

  const switchView = (v: View) => {
    setView(v)
    localStorage.setItem('view', v)
  }

  /** 拖动分隔条调整右侧编辑面板宽度 */
  const startResize = (e: React.PointerEvent) => {
    e.preventDefault()
    const startX = e.clientX
    const startW = paneW
    const move = (ev: PointerEvent) => {
      const w = Math.min(720, Math.max(300, startW + startX - ev.clientX))
      setPaneW(w)
    }
    const up = () => {
      window.removeEventListener('pointermove', move)
      window.removeEventListener('pointerup', up)
      document.body.classList.remove('resizing')
      setPaneW((w) => {
        localStorage.setItem('paneW', String(w))
        return w
      })
    }
    document.body.classList.add('resizing')
    window.addEventListener('pointermove', move)
    window.addEventListener('pointerup', up)
  }

  const reload = useCallback(async () => {
    const cfg = await api.getConfig()
    setLang(cfg.settings.language)
    setConfig(cfg)
    return cfg
  }, [])

  const flash = useCallback((msg: string) => {
    setToast(msg)
    window.setTimeout(() => setToast(''), 1800)
  }, [])

  useEffect(() => {
    void reload()
    void api.configPath().then(setConfigPath)
    void api.isEnabled().then(setEnabled)
    const offs = [
      listen('config:changed', () => void reload()),
      listen<boolean>('enabled:changed', (e) => setEnabled(e.payload)),
      listen<number>('feedback:unread', (e) => setUnread(e.payload)),
    ]
    return () => offs.forEach((p) => void p.then((off) => off()))
  }, [reload])

  useEffect(() => {
    check()
      .then((u) => u && setUpdate(u))
      .catch((e) => console.error('检查更新失败', e))
  }, [])

  const active: Profile | null = config
    ? config.profiles.find((p) => p.id === activeId) ?? config.profiles[0]
    : null

  const conflicts = useMemo(
    () =>
      active && config
        ? findConflicts(active.gestures, config.settings.match_threshold)
        : { pairs: [], ids: new Set<string>() },
    [active, config],
  )

  if (!config || !active) return null

  const onCount = active.gestures.filter((g) => g.enabled).length
  const offCount = active.gestures.length - onCount

  const needle = query.trim().toLowerCase()
  const shown = active.gestures.filter((g) => {
    if (filter === 'on' && !g.enabled) return false
    if (filter === 'off' && g.enabled) return false
    if (!needle) return true
    return `${g.name} ${describeAction(g.action)}`.toLowerCase().includes(needle)
  })

  const guard = async (fn: () => Promise<void>) => {
    try {
      await fn()
      await reload()
    } catch (e) {
      flash(String(e))
    }
  }

  const saveGesture = (gesture: Gesture) =>
    void guard(async () => {
      await api.upsertGesture(active.id, gesture)
      setEditing(view === 'list' ? { gesture, isNew: false } : null)
      flash(t('saved'))
    })

  const saveProfile = (profile: Profile) =>
    void guard(async () => {
      await api.upsertProfile(profile)
      setProfileOpen(false)
      setActiveId(profile.id)
    })

  const saveSettings = (settings: Settings) =>
    void guard(async () => {
      await api.saveSettings(settings)
      setSettingsOpen(false)
      flash(t('saved'))
    })

  const toggleMaster = async (on: boolean) => {
    await api.setEnabled(on)
    setEnabled(on)
  }

  const addGesture = () => {
    switchView('list')
    setEditing({ gesture: emptyGesture(), isNew: true })
  }

  const editGesture = (g: Gesture) => {
    switchView('list')
    setEditing({ gesture: g, isNew: false })
  }

  /** 触发键与滚轮合并成一枚徽标 */
  const triggerChip = (g: Gesture) => {
    const parts: string[] = []
    if (g.trigger) parts.push(TRIGGER_LABEL[g.trigger])
    if (g.wheel.length > 0)
      parts.push(`${t('wheel')} ${g.wheel.map((w) => (w === 'Up' ? '↑' : '↓')).join('')}`)
    return parts.join(' → ')
  }

  return (
    <div className="shell">
      <header className="topbar">
        <div className="brand">
          <Spline size={22} strokeWidth={2} />
          {t('appName')}
        </div>
        <div className="topbar-right">
          <button
            type="button"
            className={enabled ? 'switch' : 'switch off'}
            title={t('gesturesEnabled')}
            onClick={() => void toggleMaster(!enabled)}
          >
            <span className="switch-track">
              <span className="switch-knob" />
            </span>
            {enabled ? t('masterOn') : t('masterOff')}
          </button>
          <button
            type="button"
            className="btn-outline subtle"
            title={unread > 0 ? `${unread} ${t('fbUnread')}` : undefined}
            onClick={() => setSettingsOpen(true)}
          >
            <Settings2 size={16} strokeWidth={2} />
            {t('settings')}
            {unread > 0 && <span className="dot" />}
          </button>
        </div>
      </header>

      <div className="board">
        <aside className="side">
          <div className="side-label">{t('profiles')}</div>
          {config.profiles.map((p, i) => (
            <div
              key={p.id}
              role="button"
              tabIndex={0}
              className={[
                'prof',
                p.id === active.id ? 'active' : '',
                p.exe ? '' : 'global',
                i === 1 ? 'first-app' : '',
              ]
                .filter(Boolean)
                .join(' ')}
              onClick={() => {
                setActiveId(p.id)
                setEditing(null)
              }}
              onKeyDown={(e) => {
                if (e.key !== 'Enter') return
                setActiveId(p.id)
                setEditing(null)
              }}
            >
              {p.exe ? (
                <AppWindow size={15} strokeWidth={2} className="prof-icon" />
              ) : (
                <Globe size={15} strokeWidth={2} className="prof-icon" />
              )}
              <span>{p.exe ? p.name : t('global')}</span>
              <small>{p.gestures.length}</small>
              {p.exe && (
                <button
                  type="button"
                  className="btn-icon prof-del"
                  title={t('removeProfile')}
                  onClick={(e) => {
                    e.stopPropagation()
                    setConfirmRemove(p)
                  }}
                >
                  <Trash2 size={14} strokeWidth={2} />
                </button>
              )}
            </div>
          ))}
          <button type="button" className="prof add" onClick={() => setProfileOpen(true)}>
            <Plus size={15} strokeWidth={2} />
            {t('addProfile')}
          </button>
        </aside>

        <main className="main">
          <div className="head">
            <h1>{t('gestureList')}</h1>
            <p className="subtitle">{active.exe || t('globalHint')}</p>

            <div className="search">
              <Search size={14} strokeWidth={2} />
              <input
                type="text"
                value={query}
                placeholder={t('search')}
                onChange={(e) => setQuery(e.target.value)}
              />
            </div>

            <div className="segs">
              {(
                [
                  ['all', t('filterAll'), active.gestures.length],
                  ['on', t('filterOn'), onCount],
                  ['off', t('filterOff'), offCount],
                ] as [Filter, string, number][]
              ).map(([key, label, n]) => (
                <button
                  key={key}
                  type="button"
                  className={filter === key ? 'on' : ''}
                  onClick={() => setFilter(key)}
                >
                  {label} {n}
                </button>
              ))}
            </div>

            <div className="viewtog">
              <button
                type="button"
                className={view === 'list' ? 'on' : ''}
                title={t('viewList')}
                onClick={() => switchView('list')}
              >
                <List size={15} strokeWidth={2} />
              </button>
              <button
                type="button"
                className={view === 'card' ? 'on' : ''}
                title={t('viewCard')}
                onClick={() => switchView('card')}
              >
                <LayoutGrid size={15} strokeWidth={2} />
              </button>
            </div>

            <button type="button" className="btn-primary" onClick={addGesture}>
              <Plus size={16} strokeWidth={2} />
              {t('addGesture')}
            </button>
          </div>

          {conflicts.pairs.length > 0 && (
            <div className="alert">
              <TriangleAlert size={16} strokeWidth={2} />
              <span>
                <b>
                  {conflicts.pairs.length} {t('conflictHint')}
                </b>
                ：
                {conflicts.pairs
                  .slice(0, 2)
                  .map((p) => `「${p.a.name}」×「${p.b.name}」`)
                  .join('、')}
                {conflicts.pairs.length > 2 && ' …'}
              </span>
            </div>
          )}

          {view === 'list' ? (
            <div className="split" style={{ gridTemplateColumns: `1fr 8px ${paneW}px` }}>
              <div className="rows">
                <div className="rows-head">
                  <div>{t('colStroke')}</div>
                  <div>{t('colName')}</div>
                  <div>{t('colTrigger')}</div>
                  <div>{t('colAction')}</div>
                  <div className="right">{t('colOps')}</div>
                </div>
                <div className="rows-body">
                  {shown.map((g) => (
                    <div
                      key={g.id}
                      role="button"
                      tabIndex={0}
                      className={[
                        'row',
                        g.enabled ? '' : 'off',
                        conflicts.ids.has(g.id) ? 'clash' : '',
                        editing && !editing.isNew && editing.gesture.id === g.id ? 'sel' : '',
                      ]
                        .filter(Boolean)
                        .join(' ')}
                      onClick={() => setEditing({ gesture: g, isNew: false })}
                      onKeyDown={(e) => e.key === 'Enter' && setEditing({ gesture: g, isNew: false })}
                    >
                      <StrokeCanvas
                        className="rthumb"
                        points={g.points}
                        region={g.region}
                        wheel={g.wheel}
                        color={g.enabled ? config.settings.trail_color : '#a1a1a1'}
                        width={2.5}
                      />
                      <div className="rname">{g.name}</div>
                      <div className="rtags">
                        {triggerChip(g) && (
                          <span className={g.enabled ? 'tag blue' : 'tag mute'}>{triggerChip(g)}</span>
                        )}
                        {g.region && (
                          <span className={g.enabled ? 'tag' : 'tag mute'}>{t(`region${g.region}`)}</span>
                        )}
                        {g.enabled && conflicts.ids.has(g.id) && (
                          <span className="tag warn">{t('conflict')}</span>
                        )}
                        {!g.enabled && <span className="tag mute">{t('disabled')}</span>}
                      </div>
                      <div className={g.action.kind === 'open_url' ? 'rdesc url' : 'rdesc'}>
                        {describeAction(g.action)}
                      </div>
                      <div className="racts">
                        <button
                          type="button"
                          className="btn-ghost"
                          title={g.enabled ? t('disable') : t('enable')}
                          onClick={(e) => {
                            e.stopPropagation()
                            void guard(() => api.setGestureEnabled(active.id, g.id, !g.enabled))
                          }}
                        >
                          {g.enabled ? <PowerOff size={14} strokeWidth={2} /> : <Power size={14} strokeWidth={2} />}
                        </button>
                        <button
                          type="button"
                          className="btn-ghost danger"
                          disabled={!g.deletable}
                          title={g.deletable ? t('remove') : t('notDeletable')}
                          onClick={(e) => {
                            e.stopPropagation()
                            if (editing && !editing.isNew && editing.gesture.id === g.id) setEditing(null)
                            void guard(() => api.removeGesture(active.id, g.id))
                          }}
                        >
                          <Trash2 size={14} strokeWidth={2} />
                        </button>
                      </div>
                    </div>
                  ))}

                  {editing?.isNew && (
                    <div className="row draft sel">
                      <span className="rthumb pending">{t('notRecorded')}</span>
                      <div className="rname">{editing.gesture.name || t('untitled')}</div>
                      <div className="rtags">
                        <span className="tag warn">{t('draftFlag')}</span>
                      </div>
                      <div className="rdesc">{describeAction(editing.gesture.action) || '—'}</div>
                      <div className="racts" />
                    </div>
                  )}

                  {shown.length === 0 && !editing?.isNew && (
                    <div className="empty">
                      {active.gestures.length === 0 ? t('noGesture') : t('noMatch')}
                    </div>
                  )}
                </div>
              </div>

              <div className="splitter" onPointerDown={startResize} />

              {editing ? (
                <GesturePane
                  key={editing.isNew ? 'new' : editing.gesture.id}
                  initial={editing.gesture}
                  isNew={editing.isNew}
                  onSave={saveGesture}
                  onClose={() => setEditing(null)}
                  onToggle={(g) =>
                    void guard(() => api.setGestureEnabled(active.id, g.id, !g.enabled))
                  }
                  onDelete={(g) =>
                    void guard(async () => {
                      await api.removeGesture(active.id, g.id)
                      setEditing(null)
                    })
                  }
                />
              ) : (
                <aside className="pane">
                  <div className="pane-empty">
                    <p>{t('paneEmpty')}</p>
                    <button type="button" className="btn-outline btn-sm" onClick={addGesture}>
                      <Plus size={15} strokeWidth={2} />
                      {t('addGesture')}
                    </button>
                  </div>
                </aside>
              )}
            </div>
          ) : (
          <div className="cards">
            {active.gestures.length === 0 ? (
              <div className="empty">{t('noGesture')}</div>
            ) : shown.length === 0 ? (
              <div className="empty">{t('noMatch')}</div>
            ) : (
              <>
                {shown.map((g) => (
                  <article
                    key={g.id}
                    className={[
                      'card',
                      g.enabled ? '' : 'off',
                      conflicts.ids.has(g.id) ? 'clash' : '',
                    ]
                      .filter(Boolean)
                      .join(' ')}
                  >
                    <div className="card-head">
                      <StrokeCanvas
                        className="thumb"
                        points={g.points}
                        times={g.times}
                        region={g.region}
                        wheel={g.wheel}
                        color={g.enabled ? config.settings.trail_color : '#a1a1a1'}
                        width={4}
                      />
                      {!g.enabled && <span className="flag">{t('disabled')}</span>}
                      {g.enabled && conflicts.ids.has(g.id) && (
                        <span className="flag warn">
                          <TriangleAlert size={11} strokeWidth={2.4} />
                          {t('conflict')}
                        </span>
                      )}
                      <button
                        type="button"
                        className="card-more"
                        title={t('more')}
                        onClick={() => setMenuId(menuId === g.id ? '' : g.id)}
                      >
                        <MoreHorizontal size={15} strokeWidth={2} />
                      </button>
                      {menuId === g.id && (
                        <>
                          <div className="card-menu-mask" onClick={() => setMenuId('')} />
                          <div className="card-menu">
                            <button
                              type="button"
                              className="danger"
                              disabled={!g.deletable}
                              title={g.deletable ? '' : t('notDeletable')}
                              onClick={() => {
                                setMenuId('')
                                void guard(() => api.removeGesture(active.id, g.id))
                              }}
                            >
                              <Trash2 size={13} strokeWidth={2} />
                              {t('remove')}
                            </button>
                          </div>
                        </>
                      )}
                    </div>

                    <div className="card-title">{g.name}</div>

                    <div className="meta">
                      {triggerChip(g) && (
                        <span className={g.enabled ? 'tag blue' : 'tag mute'}>{triggerChip(g)}</span>
                      )}
                      {g.region && (
                        <span className={g.enabled ? 'tag' : 'tag mute'}>
                          {t(`region${g.region}`)}
                        </span>
                      )}
                      {g.fire_mode === 'Immediate' && <span className="tag yellow">{t('immediate')}</span>}
                    </div>

                    <div className={g.action.kind === 'open_url' ? 'card-desc url' : 'card-desc'}>
                      {describeAction(g.action)}
                    </div>

                    <div className="card-acts">
                      <button
                        type="button"
                        className="btn-ghost"
                        onClick={() => editGesture(g)}
                      >
                        <Pencil size={14} strokeWidth={2} />
                        {t('edit')}
                      </button>
                      <button
                        type="button"
                        className="btn-ghost"
                        onClick={() =>
                          void guard(() => api.setGestureEnabled(active.id, g.id, !g.enabled))
                        }
                      >
                        {g.enabled ? <PowerOff size={14} strokeWidth={2} /> : <Power size={14} strokeWidth={2} />}
                        {g.enabled ? t('disable') : t('enable')}
                      </button>
                    </div>
                  </article>
                ))}
                <button type="button" className="card add" onClick={addGesture}>
                  <Plus size={26} strokeWidth={1.6} />
                  {t('drawNew')}
                </button>
              </>
            )}
          </div>
          )}
        </main>
      </div>

      {profileOpen && (
        <ProfileDialog onSave={saveProfile} onClose={() => setProfileOpen(false)} />
      )}
      {settingsOpen && (
        <SettingsDialog
          initial={config.settings}
          unread={unread}
          configPath={configPath}
          onSave={saveSettings}
          onClose={() => setSettingsOpen(false)}
        />
      )}
      {confirmRemove && (
        <div className="mask" onMouseDown={(e) => e.target === e.currentTarget && setConfirmRemove(null)}>
          <div className="dialog narrow">
            <div className="dialog-head">
              <h2>{t('removeProfile')}</h2>
            </div>
            <div className="dialog-body">
              <p>{t('confirmRemoveProfile')}</p>
            </div>
            <div className="dialog-foot">
              <button type="button" className="btn-outline subtle" onClick={() => setConfirmRemove(null)}>
                {t('cancel')}
              </button>
              <button
                type="button"
                className="btn-primary danger"
                onClick={() =>
                  void guard(async () => {
                    await api.removeProfile(confirmRemove.id)
                    setActiveId('global')
                    setConfirmRemove(null)
                  })
                }
              >
                <Trash2 size={16} strokeWidth={2} />
                {t('confirm')}
              </button>
            </div>
          </div>
        </div>
      )}
      {toast && (
        <div className="toast">
          <Zap size={16} strokeWidth={2} />
          {toast}
        </div>
      )}
      {update && <UpdateDialog update={update} onClose={() => setUpdate(null)} />}
    </div>
  )
}
