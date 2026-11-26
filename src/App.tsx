import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { listen } from '@tauri-apps/api/event'
import { getVersion } from '@tauri-apps/api/app'
import { check, type Update } from '@tauri-apps/plugin-updater'
import {
  AppWindow,
  Ban,
  Check,
  Copy,
  Globe,
  MoreHorizontal,
  Plus,
  Power,
  PowerOff,
  Trash2,
  TriangleAlert,
  Zap,
} from 'lucide-react'
import {
  api,
  cloneGesture,
  DOCS_URL,
  emptyGesture,
  hasMouseTrigger,
  hotkeyLabel,
  isHotkeyOnly,
  openExternal,
  type AiAction,
  type AiSettings,
  type Config,
  type Gesture,
  type Profile,
  type Settings,
} from './api'
import { findConflicts } from './conflicts'
import { setLang, t } from './i18n'
import { describeAction } from './components/ActionEditor'
import { AiPage } from './components/AiPage'
import { GesturePane, HotkeyThumb } from './components/GesturePane'
import { ProfileDialog } from './components/ProfileDialog'
import { Rail } from './components/Rail'
import { SettingsDialog } from './components/SettingsDialog'
import { StrokeCanvas } from './components/StrokeCanvas'
import { VersionDialog } from './components/VersionDialog'

const TRIGGER_LABEL: Record<string, string> = {
  Right: '右键',
  Middle: '中键',
  X1: '侧键 1',
  X2: '侧键 2',
}

export function App() {
  const [config, setConfig] = useState<Config | null>(null)
  const [page, setPage] = useState<'gestures' | 'ai'>('gestures')
  const [configPath, setConfigPath] = useState('')
  const [activeId, setActiveId] = useState('global')
  const [editing, setEditing] = useState<{ gesture: Gesture; isNew: boolean } | null>(null)
  const [profileOpen, setProfileOpen] = useState(false)
  const [settingsOpen, setSettingsOpen] = useState(false)
  const [unread, setUnread] = useState(0)
  const [enabled, setEnabled] = useState(true)
  const [toast, setToast] = useState('')
  const [confirmRemove, setConfirmRemove] = useState<Profile | null>(null)
  const [menuId, setMenuId] = useState('')
  const [menuPos, setMenuPos] = useState({ x: 0, y: 0 })
  const [update, setUpdate] = useState<Update | null>(null)
  const [versionOpen, setVersionOpen] = useState(false)
  const [updateStatus, setUpdateStatus] = useState<'checking' | 'checked' | 'failed'>('checking')
  const checkingUpdate = useRef(false)
  const [version, setVersion] = useState('')
  const [hotkeyFailures, setHotkeyFailures] = useState<string[]>([])

  const [paneW, setPaneW] = useState(() => Number(localStorage.getItem('paneW')) || 400)

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
    // 快捷键在保存后重新注册，顺带刷新被占用的列表
    api
      .hotkeyFailures()
      .then(setHotkeyFailures)
      .catch((e) => {
        console.error('读取快捷键注册失败列表出错', e)
        setHotkeyFailures([])
      })
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

  const checkForUpdates = useCallback(async () => {
    if (checkingUpdate.current) return
    checkingUpdate.current = true
    setUpdateStatus('checking')
    try {
      setUpdate(await check({ timeout: 10_000 }))
      setUpdateStatus('checked')
    } catch (e) {
      setUpdate(null)
      setUpdateStatus('failed')
      console.error('检查更新失败', e)
    } finally {
      checkingUpdate.current = false
    }
  }, [])

  useEffect(() => {
    getVersion()
      .then(setVersion)
      .catch((e) => console.error('获取版本号失败', e))
    void checkForUpdates()
  }, [checkForUpdates])

  useEffect(() => {
    if (update) return () => {
      void update.close().catch((e) => console.error('释放更新资源失败', e))
    }
  }, [update])

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

  const activePassthrough = !!active.exe && !!active.passthrough
  const totalGestures = config.profiles.reduce((n, p) => n + p.gestures.length, 0)

  const shown = active.gestures

  const guard = async (fn: () => Promise<void>) => {
    try {
      await fn()
      await reload()
    } catch (e) {
      flash(String(e))
    }
  }

  /** 失败时把后端的错误抛回编辑面板，由面板内联显示 */
  const saveGesture = async (gesture: Gesture) => {
    await api.upsertGesture(active.id, gesture)
    setEditing({ gesture, isNew: false })
    flash(t('saved'))
    await reload().catch((e) => flash(String(e)))
  }

  const openDocs = () => void openExternal(DOCS_URL).catch((e) => flash(String(e)))

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

  /** AI 页保存：只改 settings.ai 的指定字段，失败时抛回由页面内联显示 */
  const saveAi = async (patch: Partial<AiSettings>) => {
    await api.saveSettings({ ...config.settings, ai: { ...config.settings.ai, ...patch } })
    await reload()
    flash(t('saved'))
  }

  /** 为 AI 动作新建一条手势：跳回手势页并打开新建面板 */
  const bindAiAction = (id: string) => {
    const action = (config.settings.ai.actions ?? []).find((a: AiAction) => a.id === id)
    if (!action) return
    if (activePassthrough) setActiveId('global')
    setPage('gestures')
    setEditing({ gesture: { ...emptyGesture(), name: action.name, action: { kind: 'ai_custom', id: action.id } }, isNew: true })
  }

  const toggleMaster = async (on: boolean) => {
    await api.setEnabled(on)
    setEnabled(on)
  }

  const addGesture = () => {
    setEditing({ gesture: emptyGesture(), isNew: true })
  }

  const duplicateGesture = (g: Gesture) => {
    setEditing({ gesture: cloneGesture(g, t('copySuffix')), isNew: true })
  }

  /** 触发键与滚轮合并成一枚徽标 */
  const triggerChip = (g: Gesture) => {
    const parts: string[] = []
    if (g.trigger) parts.push(TRIGGER_LABEL[g.trigger])
    if (g.region && g.trigger && g.points.length === 0 && g.wheel.length === 0) parts.push(t('click'))
    if (g.dwell_ms > 0) parts.push(`${t('dwell')} ${g.dwell_ms}ms`)
    if (g.wheel.length > 0)
      parts.push(`${t('wheel')} ${g.wheel.map((w) => (w === 'Up' ? '↑' : '↓')).join('')}`)
    if (g.left_click) parts.push(t('leftClick'))
    return parts.join(' → ')
  }

  /** 触发列在鼠标徽标后追加键帽样式的快捷键标签 */
  const hotkeyTag = (g: Gesture) => {
    const label = hotkeyLabel(g.hotkey)
    if (!label) return null
    const busy = hotkeyFailures.includes(label)
    return (
      <span
        className={!g.enabled ? 'tag kb mute' : busy ? 'tag kb busy' : 'tag kb'}
        title={busy ? t('hotkeyBusy') : undefined}
      >
        {label}
      </span>
    )
  }

  return (
    <div className="shell">
      <Rail
        page={page}
        onPage={setPage}
        gestureCount={totalGestures}
        aiCount={config.settings.ai.actions?.length ?? 0}
        enabled={enabled}
        onToggleEnabled={() => void toggleMaster(!enabled).catch((e) => flash(String(e)))}
        unread={unread}
        onSettings={() => setSettingsOpen(true)}
        onDocs={openDocs}
        version={version}
        update={update}
        onVersion={() => {
          setVersionOpen(true)
          void checkForUpdates()
        }}
      />

      {page === 'ai' && (
        <AiPage
          saved={config.settings.ai}
          profiles={config.profiles}
          onSaveActions={(actions) => saveAi({ actions })}
          onSaveService={saveAi}
          onBind={bindAiAction}
        />
      )}

      <div className="board" hidden={page !== 'gestures'}>
        <aside className="side">
          <div className="side-scroll">
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
                {p.exe && p.passthrough && <Ban size={14} className="prof-icon" aria-label={t('passthroughOn')} />}
                <small>{p.gestures.length}</small>
                {p.exe && (
                  <>
                    <button
                      type="button"
                      className={menuId === p.id ? 'btn-icon prof-del open' : 'btn-icon prof-del'}
                      title={t('more')}
                      onClick={(e) => {
                        e.stopPropagation()
                        const r = e.currentTarget.getBoundingClientRect()
                        setMenuPos({ x: r.right + 10, y: Math.max(8, Math.min(r.top, window.innerHeight - 140)) })
                        setMenuId(menuId === p.id ? '' : p.id)
                      }}
                    >
                      <MoreHorizontal size={14} strokeWidth={2} />
                    </button>
                    {menuId === p.id && (
                      <>
                        <div
                          className="prof-menu-mask"
                          onClick={(e) => {
                            e.stopPropagation()
                            setMenuId('')
                          }}
                        />
                        <div className="prof-menu" style={{ left: menuPos.x, top: menuPos.y }} onClick={(e) => e.stopPropagation()}>
                          {([false, true] as const).map((pass) => (
                            <button
                              key={String(pass)}
                              type="button"
                              onClick={() => {
                                setMenuId('')
                                void guard(async () => {
                                  await api.setProfilePassthrough(p.id, pass)
                                  flash(pass ? t('passthroughOn') : t('passthroughOff'))
                                })
                              }}
                            >
                              <Check size={13} strokeWidth={2.4} className={!!p.passthrough === pass ? '' : 'hid'} />
                              {pass ? t('modePassthrough') : t('modeNative')}
                            </button>
                          ))}
                          <button
                            type="button"
                            className="danger"
                            onClick={() => {
                              setMenuId('')
                              setConfirmRemove(p)
                            }}
                          >
                            <Trash2 size={13} strokeWidth={2} />
                            {t('removeProfile')}
                          </button>
                        </div>
                      </>
                    )}
                  </>
                )}
              </div>
            ))}
            <button type="button" className="prof add" onClick={() => setProfileOpen(true)}>
              <Plus size={15} strokeWidth={2} />
              {t('addProfile')}
            </button>
          </div>
        </aside>

        <main className={activePassthrough ? 'main dim' : 'main'}>

          {activePassthrough && (
            <div className="banner" role="status">
              <Ban size={16} strokeWidth={2} className="banner-icon" />
              <span>
                {active.name} {t('passthroughBanner')}
              </span>
            </div>
          )}
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
                      {isHotkeyOnly(g) ? (
                        <HotkeyThumb className="rthumb" />
                      ) : (
                        <StrokeCanvas
                          className="rthumb"
                          points={g.points}
                          trigger={g.trigger}
                          leftClick={g.left_click}
                          region={g.region}
                          wheel={g.wheel}
                          color={g.enabled ? config.settings.trail_color : '#a1a1a1'}
                          width={2.5}
                        />
                      )}
                      <div className="rname">{g.name}</div>
                      <div className="rtags">
                        {triggerChip(g) && (
                          <span className={g.enabled ? 'tag blue' : 'tag mute'}>{triggerChip(g)}</span>
                        )}
                        {g.region && (
                          <span className={g.enabled ? 'tag' : 'tag mute'}>{t(`region${g.region}`)}</span>
                        )}
                        {hotkeyTag(g)}
                        {g.enabled && conflicts.ids.has(g.id) && (
                          <span className="tag warn">{t('conflict')}</span>
                        )}
                        {!g.enabled && <span className="tag mute">{t('disabled')}</span>}
                      </div>
                      <div className={g.action.kind === 'open_url' ? 'rdesc url' : 'rdesc'}>
                        {describeAction(g.action, config.settings.ai.actions ?? [])}
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
                          className="btn-ghost"
                          title={t('duplicate')}
                          onClick={(e) => {
                            e.stopPropagation()
                            duplicateGesture(g)
                          }}
                        >
                          <Copy size={14} strokeWidth={2} />
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
                      {isHotkeyOnly(editing.gesture) ? (
                        <HotkeyThumb className="rthumb" />
                      ) : hasMouseTrigger(editing.gesture) ? (
                        <StrokeCanvas
                          className="rthumb"
                          points={editing.gesture.points}
                          trigger={editing.gesture.trigger}
                          leftClick={editing.gesture.left_click}
                          region={editing.gesture.region}
                          wheel={editing.gesture.wheel}
                          color={config.settings.trail_color}
                          width={2.5}
                        />
                      ) : (
                        <span className="rthumb pending">{t('notRecorded')}</span>
                      )}
                      <div className="rname">{editing.gesture.name || t('untitled')}</div>
                      <div className="rtags">
                        <span className="tag warn">{t('draftFlag')}</span>
                        {hotkeyTag(editing.gesture)}
                      </div>
                      <div className="rdesc">{describeAction(editing.gesture.action, config.settings.ai.actions ?? []) || '—'}</div>
                      <div className="racts" />
                    </div>
                  )}

                  {shown.length === 0 && !editing?.isNew && (
                    <div className="empty">
                      {t('noGesture')}
                    </div>
                  )}
                </div>
              </div>

              <div className="splitter" onPointerDown={startResize} />

              {editing ? (
                <GesturePane
                  key={`${editing.isNew ? 'new' : 'edit'}:${editing.gesture.id}`}
                  aiActions={config.settings.ai.actions ?? []}
                  initial={editing.gesture}
                  isNew={editing.isNew}
                  onSave={saveGesture}
                  hotkeyFailures={hotkeyFailures}
                  onClose={() => setEditing(null)}
                  onDuplicate={duplicateGesture}
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
      {versionOpen && (
        <VersionDialog
          version={version}
          update={update}
          updateStatus={updateStatus}
          onCheck={checkForUpdates}
          onClose={() => setVersionOpen(false)}
        />
      )}
    </div>
  )
}
