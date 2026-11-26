import { useEffect, useMemo, useRef, useState } from 'react'
import { Check, Languages, PenLine, Plug, Plus, Search, TextCursorInput, Trash2, Type, X, type LucideIcon } from 'lucide-react'
import type { AiAction, AiSettings, Profile } from '../api'
import { ALL_AI_ICON_NAMES, COMMON_AI_ICONS, DEFAULT_AI_ICON, aiIcon } from './aiIcons'

const SELECTION = '{{selection}}'

const BUILTIN: { name: string; icon: LucideIcon }[] = [
  { name: 'AI 翻译', icon: Languages },
  { name: 'AI 润色', icon: PenLine },
  { name: '粤拼', icon: Type },
]

interface Props {
  saved: AiSettings
  profiles: Profile[]
  onSaveActions: (actions: AiAction[]) => Promise<void>
  onSaveService: (patch: Partial<AiSettings>) => Promise<void>
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
            <input value={q} placeholder="搜索 lucide 图标，如 mail" onChange={(e) => setQ(e.target.value)} />
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
    show_polish_notes: saved.show_polish_notes,
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
            <input id="ai-model" value={s.model} spellCheck={false}
              onChange={(e) => setS({ ...s, model: e.target.value })} /></div>
          <label className="check field">
            <input type="checkbox" checked={s.show_polish_notes}
              onChange={(e) => setS({ ...s, show_polish_notes: e.target.checked })} />
            显示英语优化提示
          </label>
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
export function AiPage({ saved, profiles, onSaveActions, onSaveService, onBind }: Props) {
  const [list, setList] = useState<AiAction[]>(saved.actions ?? [])
  const [selected, select] = useState(saved.actions?.[0]?.id ?? '')
  const [sample, setSample] = useState('报价单今天发我一下。')
  const [removing, setRemoving] = useState(false)
  const [serviceOpen, setServiceOpen] = useState(false)
  const [error, setError] = useState('')
  const [busy, setBusy] = useState(false)
  const input = useRef<HTMLTextAreaElement>(null)

  const dirty = JSON.stringify(list) !== JSON.stringify(saved.actions ?? [])
  const dirtyRef = useRef(dirty)
  dirtyRef.current = dirty
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

  const add = () => {
    const id = crypto.randomUUID()
    setList([...list, { id, name: '', prompt: SELECTION, mode: 'bubble', icon: '' }])
    select(id)
    setRemoving(false)
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

  const configured = !!saved.base_url && !!saved.api_key

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
          {list.length === 0 && <p className="field-note">还没有自定义动作，点右上角「新增动作」。</p>}
          {list.map((a) => {
            const Icon = aiIcon(a.icon)
            const n = countBindings(a.id).length
            return (
              <button key={a.id} type="button" className={a.id === action?.id ? 'ai-item on' : 'ai-item'}
                onClick={() => { select(a.id); setRemoving(false); setError('') }}>
                <span className="ai-ico"><Icon size={18} strokeWidth={2} /></span>
                <span className="ai-item-text">
                  <b>{a.name || '新动作'}</b>
                  <small>{a.mode === 'replace' ? '替换选中文字' : '气泡显示'} · {n ? `${n} 个手势` : '未绑定'}</small>
                </span>
              </button>
            )
          })}
          <div className="side-label">内置</div>
          {BUILTIN.map(({ name, icon: Icon }) => (
            <div key={name} className="ai-item lock">
              <span className="ai-ico"><Icon size={18} strokeWidth={2} /></span>
              <span className="ai-item-text"><b>{name}</b><small>内置</small></span>
            </div>
          ))}
        </aside>

        {action ? (
          <section className="ai-edit">
            <div className="ai-form">
              <div className="field"><label htmlFor="ai-name">名称</label>
                <input id="ai-name" value={action.name} maxLength={80} autoFocus={!action.name}
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
              <p className="field-note">{bindings.length ? '已绑定手势：' + bindings.join('、') : '尚未绑定手势'}</p>
              {error && <p className="error">{error}</p>}
              {removing && (
                <p className="field-note">删除「{action.name || '新动作'}」？保存后，相关手势将解除绑定并停用。{' '}
                  <button type="button" className="btn-link" onClick={() => {
                    const rest = list.filter((a) => a.id !== action.id)
                    setList(rest)
                    select(rest[0]?.id ?? '')
                    setRemoving(false)
                  }}>确认删除</button>{' '}
                  <button type="button" className="btn-link" onClick={() => setRemoving(false)}>取消</button></p>
              )}
              <div className="ai-foot">
                <button type="button" className="btn-outline" onClick={() => setRemoving(true)}><Trash2 size={16} />删除</button>
                <span className="grow" />
                <button type="button" className="btn-outline" disabled={busy} onClick={() => void save(true)}>保存并绑定手势</button>
                <button type="button" className="btn-primary" disabled={busy || !dirty} onClick={() => void save()}>
                  <Check size={16} />保存</button>
              </div>
            </div>

            <div className="ai-preview">
              <div className="side-label">实时预览</div>
              <div className="field"><label htmlFor="ai-sample">示例选文</label>
                <input id="ai-sample" value={sample} onChange={(e) => setSample(e.target.value)} /></div>
              <div className="side-label">最终发送的 Prompt</div>
              <pre className="ai-bubble">{action.prompt.split(SELECTION).join(sample)}</pre>
              <p className="field-note">{action.mode === 'replace' ? '结果将替换选中文字' : '结果将以气泡显示'}</p>
            </div>
          </section>
        ) : (
          <section className="ai-edit empty"><p>选择左侧动作，或新增一个自定义 AI 动作。</p></section>
        )}
      </div>

      {serviceOpen && <ServiceDialog saved={saved} onSave={onSaveService} onClose={() => setServiceOpen(false)} />}
    </div>
  )
}
