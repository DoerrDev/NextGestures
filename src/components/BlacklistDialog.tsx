import { Ban, X } from 'lucide-react'
import { t } from '../i18n'
import { BlacklistPane } from './BlacklistPane'

export function BlacklistDialog({ names, onChange, onClose }: {
  names: string[]
  onChange: (names: string[]) => Promise<void>
  onClose: () => void
}) {
  return (
    <div className="mask" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <div className="dialog narrow" role="dialog" aria-modal="true" aria-labelledby="blacklist-title">
        <div className="dialog-head">
          <h2 id="blacklist-title"><Ban size={18} /> {t('tabBlacklist')}</h2>
          <button type="button" className="btn-icon" onClick={onClose} title={t('close')}><X size={16} /></button>
        </div>
        <div className="dialog-body"><BlacklistPane names={names} onChange={onChange} /></div>
        <div className="dialog-foot">
          <button type="button" className="btn-outline subtle" onClick={onClose}>{t('close')}</button>
        </div>
      </div>
    </div>
  )
}
