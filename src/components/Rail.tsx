import type { Update } from '@tauri-apps/plugin-updater'
import { BookOpen, Settings2, Sparkles, Spline } from 'lucide-react'
import { t } from '../i18n'

interface Props {
  page: 'gestures' | 'ai'
  onPage: (page: 'gestures' | 'ai') => void
  gestureCount: number
  aiCount: number
  enabled: boolean
  onToggleEnabled: () => void
  unread: number
  onSettings: () => void
  onDocs: () => void
  version: string
  update: Update | null
  onVersion: () => void
}

/** 左侧导航栏：上面是页面导航，下面是总开关、文档、设置和版本号 */
export function Rail({
  page,
  onPage,
  gestureCount,
  aiCount,
  enabled,
  onToggleEnabled,
  unread,
  onSettings,
  onDocs,
  version,
  update,
  onVersion,
}: Props) {
  return (
    <nav className="rail" aria-label={t('appName')}>
      <span className="rail-logo" aria-hidden="true">
        N
      </span>
      <button type="button" className={page === 'gestures' ? 'rail-nav on' : 'rail-nav'}
        aria-current={page === 'gestures' ? 'page' : undefined} onClick={() => onPage('gestures')}>
        <Spline size={19} strokeWidth={2} />
        {t('navGestures')}
        <span className="rail-badge">{gestureCount}</span>
      </button>
      <button type="button" className={page === 'ai' ? 'rail-nav on' : 'rail-nav'}
        aria-current={page === 'ai' ? 'page' : undefined} onClick={() => onPage('ai')}>
        <Sparkles size={19} strokeWidth={2} />
        {t('navAi')}
        <span className="rail-badge">{aiCount}</span>
      </button>

      <span className="rail-grow" />

      <button
        type="button"
        role="switch"
        aria-checked={enabled}
        className={enabled ? 'rail-power' : 'rail-power off'}
        title={enabled ? t('masterPauseHint') : t('masterResumeHint')}
        onClick={onToggleEnabled}
      />
      <span className="rail-power-label">{enabled ? t('masterOn') : t('masterOff')}</span>

      <button type="button" className="rail-nav" title={t('docsHint')} onClick={onDocs}>
        <BookOpen size={19} strokeWidth={2} />
        {t('docs')}↗
      </button>
      <button
        type="button"
        className="rail-nav"
        title={unread > 0 ? `${unread} ${t('fbUnread')}` : t('settings')}
        onClick={onSettings}
      >
        <Settings2 size={19} strokeWidth={2} />
        {t('settings')}
        {unread > 0 && <span className="dot rail-dot" />}
      </button>
      {version && (
        <button
          type="button"
          className={update ? 'rail-version has-update' : 'rail-version'}
          title={update ? `${t('updateHint')} v${update.version}` : t('versionHint')}
          aria-haspopup="dialog"
          onClick={onVersion}
        >
          v{version}
          {update && <span className="dot rail-dot" />}
        </button>
      )}
    </nav>
  )
}
