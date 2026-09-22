import { useEffect, useRef, useState } from 'react'
import { RefreshCw, Send } from 'lucide-react'
import { api, type ChatItem } from '../api'
import { t } from '../i18n'

interface Props {
  nickname: string
  onNickname: (name: string) => void
}

/** 服务端时间是无时区后缀的 UTC，补 Z 后按本地显示 */
function fmtTime(iso: string): string {
  const d = new Date(iso.endsWith('Z') ? iso : `${iso}Z`)
  if (Number.isNaN(d.getTime())) return iso
  return d.toLocaleString(undefined, { hour12: false })
}

export function FeedbackPane({ nickname, onNickname }: Props) {
  const [items, setItems] = useState<ChatItem[] | null>(null)
  const [loadError, setLoadError] = useState('')
  const [status, setStatus] = useState(t('fbSyncing'))
  const [message, setMessage] = useState('')
  const [sending, setSending] = useState(false)
  const [error, setError] = useState('')
  const listRef = useRef<HTMLDivElement>(null)

  const load = async (silent = false) => {
    if (!silent) setStatus(t('fbSyncing'))
    try {
      const rows = await api.feedbackChat()
      setItems(rows)
      setLoadError('')
      setStatus(t('fbSynced'))
      if (rows.some((r) => r.type === 'admin' && !r.is_read)) void api.feedbackAckUnread()
    } catch (e) {
      if (silent) return
      setLoadError(String(e))
      setStatus(t('fbSyncFailed'))
    }
  }

  useEffect(() => {
    void load()
    const timer = window.setInterval(() => void load(true), 8000)
    return () => window.clearInterval(timer)
  }, [])

  useEffect(() => {
    const el = listRef.current
    if (el) el.scrollTop = el.scrollHeight
  }, [items])

  const submit = async () => {
    const name = nickname.trim()
    const text = message.trim()
    if (!name) return setError(t('fbNeedName'))
    if (!text) return setError(t('fbNeedMessage'))
    setError('')
    setSending(true)
    try {
      await api.feedbackSubmit(name, text)
      setMessage('')
      await load()
    } catch (e) {
      setError(t('fbSendFailed') + String(e))
    } finally {
      setSending(false)
    }
  }

  return (
    <div className="fb">
      <div className="field">
        <label htmlFor="fb-name">{t('fbNickname')}</label>
        <input
          id="fb-name"
          type="text"
          maxLength={64}
          placeholder={t('fbNicknamePlaceholder')}
          value={nickname}
          onChange={(e) => onNickname(e.target.value)}
        />
      </div>

      <div className="fb-panel">
        <div className="fb-panel-head">
          <span>{t('fbHistory')}</span>
          <span className="fb-status">{status}</span>
          <button type="button" className="btn-icon" title={t('fbRefresh')} onClick={() => void load()}>
            <RefreshCw size={14} strokeWidth={2} />
          </button>
        </div>
        <div className="fb-list" ref={listRef}>
          {loadError && <p className="error">{t('fbLoadFailed') + loadError}</p>}
          {items && items.length === 0 && !loadError && <p className="fb-empty">{t('fbEmpty')}</p>}
          {items?.map((m) => {
            const incoming = m.type !== 'user'
            return (
              <div key={`${m.type}-${m.id}`} className={incoming ? 'fb-msg in' : 'fb-msg out'}>
                <span className="fb-meta">
                  {incoming ? t('fbDev') : m.user_name || t('fbMe')} · {fmtTime(m.created_at)}
                </span>
                <div className="fb-bubble">{m.content}</div>
              </div>
            )
          })}
        </div>
      </div>

      <div className="field">
        <label htmlFor="fb-msg">{t('fbCompose')}</label>
        <textarea
          id="fb-msg"
          rows={4}
          maxLength={4000}
          placeholder={t('fbComposePlaceholder')}
          value={message}
          onChange={(e) => setMessage(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === 'Enter' && e.ctrlKey) {
              e.preventDefault()
              void submit()
            }
          }}
        />
        <p className="field-note" style={{ marginTop: 6 }}>
          {t('fbComposeHint')}
        </p>
      </div>

      <div className="fb-actions">
        {error && <span className="error">{error}</span>}
        <button type="button" className="btn-primary" disabled={sending} onClick={() => void submit()}>
          <Send size={16} strokeWidth={2} />
          {sending ? t('fbSending') : t('fbSend')}
        </button>
      </div>
    </div>
  )
}
