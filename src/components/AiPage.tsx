import { useEffect, useLayoutEffect, useMemo, useRef, useState } from 'react'
import { Check, Plug, Plus, Search, TextCursorInput, Trash2, X } from 'lucide-react'
import type { AiAction, AiSettings, Profile } from '../api'
import { ALL_AI_ICON_NAMES, COMMON_AI_ICONS, DEFAULT_AI_ICON, aiIcon } from './aiIcons'

const SELECTION = '{{selection}}'

interface Props {
  saved: AiSettings
  profiles: Profile[]
  onSaveActions: (actions: AiAction[]) => Promise<void>
  onRemoveAction: (id: string) => Promise<void>
  onSaveService: (patch: Partial<AiSettings>) => Promise<void>
  onProtectDraft: (protectedDraft: boolean) => void
  /** 保存完成后，新建一条绑定该动作的手势 */
  onBind: (id: string) => void
}

function IconPicker({ value, onChange }: { value: string; onChange: (icon: string) => void }) {
  const [more, setMore] = useState(false)
  const [q, setQ] = useState('')
  const current = value || DEFAULT_AI_ICON
  const shown = COMMON_AI_ICONS.includes(current) ? COMMON_AI_ICONS : [current, ...COMMON_AI_ICONS]
  const results = useMemo(() => {
    const key = q.trim().toLowerCase()
    return (key ? ALL_AI_ICON_NAMES.filter((n) => n.toLowerCase().includes(key)) : ALL_AI_ICON_NAMES).slice(0, 72)
  }, [q])
  const cell = (name: string) => {
    const Icon = aiIcon(name)
    const on = current === name
    return (
      <button key={name} type="button" className={on ? 'ai-ico on' : 'ai-ico'} title={name} aria-pressed={on}
        onClick={() => onChange(name)}>
        <Icon size={18} strokeWidth={2} />
      </button>
    )
  }
  return (
    <div>
      <div className="ai-icons">
        {shown.map(cell)}
        <button type="button" className={more ? 'ai-ico on' : 'ai-ico'} title="更多图标" aria-expanded={more}
          onClick={() => setMore(!more)}>…</button>
      </div>
      {more && (
        <div className="ai-icon-more">
          <label className="ai-search"><Search size={14} />
            <input type="text" value={q} placeholder="搜索图标，如 Mail" onChange={(e) => setQ(e.target.value)} />
          </label>
          <div className="ai-icons">{results.map(cell)}</div>
        </div>
      )}
    </div>
  )
}

function ServiceDialog({ saved, onSave, onClose }: {
  saved: AiSettings
  onSave: (patch: Partial<AiSettings>) => Promise<void>
  onClose: () => void
}) {
  const [s, setS] = useState({
    base_url: saved.base_url,
    api_key: saved.api_key,
    model: saved.model,
  })
  const [error, setError] = useState('')
  const submit = async () => {
    try {
      await onSave(s)
      onClose()
    } catch (e) {
      setError(String(e))
    }
  }
  return (
    <div className="mask" onMouseDown={(e) => e.target === e.currentTarget && onClose()}>
      <div className="dialog narrow">
        <div className="dialog-head">
          <h2>AI 服务</h2>
          <button type="button" className="btn-icon" onClick={onClose} title="关闭"><X size={16} /></button>
        </div>
        <div className="dialog-body">
          <div className="field"><label htmlFor="ai-base-url">OpenAI 兼容接口地址</label>
            <input id="ai-base-url" type="url" value={s.base_url} placeholder="https://api.openai.com/v1" spellCheck={false}
              onChange={(e) => setS({ ...s, base_url: e.target.value })} /></div>
          <div className="field"><label htmlFor="ai-api-key">API 密钥</label>
            <input id="ai-api-key" type="password" value={s.api_key} autoComplete="off"
              onChange={(e) => setS({ ...s, api_key: e.target.value })} /></div>
          <div className="field"><label htmlFor="ai-model">模型</label>
            <input id="ai-model" type="text" value={s.model} spellCheck={false}
              onChange={(e) => setS({ ...s, model: e.target.value })} /></div>
          <p className="field-note">触发 AI 动作时，选中文字将发送到这里配置的服务。</p>
          {error && <p className="error">{error}</p>}
        </div>
        <div className="dialog-foot">
          <button type="button" className="btn-outline subtle" onClick={onClose}>取消</button>
          <button type="button" className="btn-primary" onClick={() => void submit()}><Check size={16} />保存</button>
        </div>
      </div>
    </div>
  )
}

/** 「AI 动作」页：左侧动作库，右侧编辑器与实时预览 */
export function AiPage({ saved, profiles, onSaveActions, onRemoveAction, onSaveService, onBind, onProtectDraft }: Props) {
  const [list, setList] = useState<AiAction[]>(saved.actions ?? [])
  const [selected, select] = useState(saved.actions?.[0]?.id ?? '')
  const [sample, setSample] = useState('报价单今天发我一下。')
  const deleteDialog = useRef<HTMLDialogElement>(null)
  const [deleting, setDeleting] = useState(false)
  const [deleteError, setDeleteError] = useState('')
  const [serviceOpen, setServiceOpen] = useState(false)
  const [error, setError] = useState('')
  const [busy, setBusy] = useState(false)
  const input = useRef<HTMLTextAreaElement>(null)

  const dirty = JSON.stringify(list) !== JSON.stringify(saved.actions ?? [])
  const dirtyRef = useRef(dirty)
  dirtyRef.current = dirty
  useLayoutEffect(() => {
    onProtectDraft(dirty || serviceOpen || busy || deleting)
    return () => onProtectDraft(false)
  }, [onProtectDraft, dirty, serviceOpen, busy, deleting])
  // 后端配置变化（例如别处保存）时，没有未保存改动才同步
  useEffect(() => {
    if (!dirtyRef.current) setList(saved.actions ?? [])
  }, [saved.actions])

  const action = list.find((a) => a.id === selected) ?? list[0]
  const update = (change: Partial<AiAction>) => {
    setError('')
    setList(list.map((a) => (a.id === action.id ? { ...a, ...change } : a)))
  }
  const countBindings = (id: string) =>
    profiles.flatMap((p) => p.gestures
      .filter((g) => g.action.kind === 'ai_custom' && g.action.id === id)
      .map((g) => `${p.exe ? p.name : 'Global'} / ${g.name}`))
  const bindings = action ? countBindings(action.id) : []
  const persisted = action && saved.actions?.some((a) => a.id === action.id)

  const remove = async () => {
    if (!action || deleting) return
    setDeleteError('')
    setDeleting(true)
    try {
      if (persisted) await onRemoveAction(action.id)
      deleteDialog.current?.close()
      setList((current) => current.filter((a) => a.id !== action.id))
      select('')
      setError('')
    } catch (e) {
      setDeleteError(`删除失败：${e}`)
    } finally {
      setDeleting(false)
    }
  }

  const add = () => {
    const id = crypto.randomUUID()
    setList([...list, { id, name: '', prompt: SELECTION, mode: 'bubble', icon: '', notes: false }])
    select(id)
    setError('')
  }
  const insert = () => {
    const field = input.current
    if (!field || !action) return
    const at = field.selectionStart
    update({ prompt: action.prompt.slice(0, at) + SELECTION + action.prompt.slice(field.selectionEnd) })
    requestAnimationFrame(() => {
      field.focus()
      field.setSelectionRange(at + SELECTION.length, at + SELECTION.length)
    })
  }

  const save = async (bindAfter?: boolean) => {
    const bad = list.find((a) => !a.name.trim() || !a.prompt.includes(SELECTION))
    if (bad) {
      select(bad.id)
      setError(`请为「${bad.name || '新动作'}」填写名称，并在 Prompt 中插入选中文字`)
      return
    }
    setBusy(true)
    try {
      await onSaveActions(list)
      if (bindAfter && action) onBind(action.id)
    } catch (e) {
      setError(String(e))
    } finally {
      setBusy(false)
    }
  }

  // 与后端一致：地址和模型必填，API 密钥可留空
  const configured = !!saved.base_url.trim() && !!saved.model.trim()

  return (
    <div className="ai-page">
      <header className="ai-top">
        <h2>AI 动作</h2>
        <span className={configured ? 'ai-chip ok' : 'ai-chip'}>
          <span className="ai-dot" />{configured ? saved.model || '已配置' : '服务未配置'}
        </span>
        <span className="grow" />
        <button type="button" className="btn-outline btn-sm" onClick={() => setServiceOpen(true)}>
          <Plug size={14} strokeWidth={2} />AI 服务
        </button>
        <button type="button" className="btn-primary btn-sm" onClick={add}>
          <Plus size={14} strokeWidth={2} />新增动作
        </button>
      </header>

      <div className="ai-body">
        <aside className="ai-list">
          <div className="side-label">我的动作</div>
          {list.length === 0 && <p className="field-note">还没有动作，点右上角「新增动作」。</p>}
          {list.map((a) => {
            const Icon = aiIcon(a.icon)
            const n = countBindings(a.id).length
            return (
              <button key={a.id} type="button" className={a.id === action?.id ? 'ai-item on' : 'ai-item'}
                onClick={() => { select(a.id); setError('') }}>
                <span className="ai-ico"><Icon size={18} strokeWidth={2} /></span>
                <span className="ai-item-text">
                  <b>{a.name || '新动作'}</b>
                  <small>{a.mode === 'replace' ? '替换选中文字' : '气泡显示'} · {n ? `${n} 个手势` : '未绑定'}</small>
                </span>
              </button>
            )
          })}
        </aside>

        {action ? (
          <section className="ai-edit">
            <div className="ai-form">
              <div className="field"><label htmlFor="ai-name">名称</label>
                <input id="ai-name" type="text" value={action.name} maxLength={80} autoFocus={!action.name}
                  onChange={(e) => update({ name: e.target.value })} /></div>
              <div className="field"><label>图标</label>
                <IconPicker value={action.icon ?? ''} onChange={(icon) => update({ icon })} /></div>
              <div className="field">
                <div className="ai-row"><label htmlFor="custom-prompt">Prompt</label>
                  <button type="button" className="btn-outline btn-sm" onMouseDown={(e) => e.preventDefault()} onClick={insert}>
                    <TextCursorInput size={14} />插入选中文字
                  </button></div>
                <textarea ref={input} id="custom-prompt" rows={7} value={action.prompt}
                  onChange={(e) => update({ prompt: e.target.value })} />
              </div>
              <div className="field"><label>结果处理</label>
                <div className="ai-seg" role="radiogroup">
                  {(['bubble', 'replace'] as const).map((m) => (
                    <button key={m} type="button" role="radio" aria-checked={action.mode === m}
                      className={action.mode === m ? 'on' : ''} onClick={() => update({ mode: m })}>
                      {m === 'bubble' ? '气泡显示' : '替换选中文字'}
                    </button>
                  ))}
                </div></div>
              <div className="field">
                <label className="check ai-check">
                  <input type="checkbox" checked={!!action.notes} onChange={(e) => update({ notes: e.target.checked })} />
                  单独显示说明
                </label>
                <p className="field-note">回复中单独一行 <code>&lt;notes&gt;</code> 之后的内容作为说明卡片显示；替换选中文字时只粘贴它之前的正文。需要在 Prompt 里要求 AI 按此格式输出。</p>
              </div>
              <p className="field-note">{bindings.length ? '已绑定手势：' + bindings.join('、') : '尚未绑定手势'}</p>
              {error && <p className="error">{error}</p>}
              <div className="ai-foot">
                <button type="button" className="btn-outline" disabled={busy} onClick={() => {
                  setDeleteError('')
                  deleteDialog.current?.showModal()
                }}><Trash2 size={16} />删除</button>
                <span className="grow" />
                <button type="button" className="btn-outline" disabled={busy} onClick={() => void save(true)}>保存并绑定手势</button>
                <button type="button" className="btn-primary" disabled={busy || !dirty} onClick={() => void save()}>
                  <Check size={16} />保存</button>
              </div>
            </div>

            <div className="ai-preview">
              <div className="side-label">实时预览</div>
              <div className="field"><label htmlFor="ai-sample">示例选文</label>
                <input id="ai-sample" type="text" value={sample} onChange={(e) => setSample(e.target.value)} /></div>
              <div className="side-label">最终发送的 Prompt</div>
              <pre className="ai-bubble">{action.prompt.split(SELECTION).join(sample)}</pre>
              <p className="field-note">
                {action.mode === 'replace' ? '结果将替换选中文字' : '结果将以气泡显示'}
                {action.notes ? '，说明单独显示' : ''}
              </p>
            </div>
          </section>
        ) : (
          <section className="ai-edit empty"><p>选择左侧动作，或新增一个 AI 动作。</p>
            {dirty && <button type="button" className="btn-primary" disabled={busy} onClick={() => void save()}><Check size={16} />保存</button>}
            {error && <p className="error">{error}</p>}
          </section>
        )}
      </div>

      {action && <dialog ref={deleteDialog} className="dialog ai-delete-dialog" aria-labelledby="delete-action-title" aria-describedby="delete-action-description"
        onCancel={(e) => { if (deleting) e.preventDefault() }}>
        <div className="dialog-head"><h2 id="delete-action-title">删除 AI 动作</h2></div>
        <div className="dialog-body" id="delete-action-description">
          确认删除「<strong>{action.name || '新动作'}</strong>」？
          {bindings.length > 0 && <p>绑定此动作的 {bindings.length} 个手势将解除绑定并停用。</p>}
          {deleteError && <p className="error" role="alert">{deleteError}</p>}
        </div>
        <div className="dialog-foot">
          <button type="button" className="btn-outline" autoFocus disabled={deleting} onClick={() => deleteDialog.current?.close()}>取消</button>
          <button type="button" className="btn-primary danger" disabled={deleting} onClick={() => void remove()}>
            <Trash2 size={16} />{deleting ? '正在删除…' : '确认删除'}
          </button>
        </div>
      </dialog>}
      {serviceOpen && <ServiceDialog saved={saved} onSave={onSaveService} onClose={() => setServiceOpen(false)} />}
    </div>
  )
}
