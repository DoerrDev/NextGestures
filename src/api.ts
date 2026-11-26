import { invoke, isTauri } from '@tauri-apps/api/core'

export type TriggerButton = 'Right' | 'Middle' | 'X1' | 'X2'
export type Region =
  | 'TopLeft'
  | 'Top'
  | 'TopRight'
  | 'Right'
  | 'BottomRight'
  | 'Bottom'
  | 'BottomLeft'
  | 'Left'
export type WheelDir = 'Up' | 'Down'
export type FireMode = 'OnRelease' | 'Immediate'
export type Point = [number, number]

export interface KeySequenceEvent {
  vk: number
  scan: number
  extended: boolean
  down: boolean
  delay_ms: number
  label: string
}

export type Action =
  | { kind: 'none' }
  | { kind: 'screenshot' }
  | { kind: 'record_region' }
  | { kind: 'ai_custom'; id: string }
  | { kind: 'command'; program: string; args: string[]; shell: boolean }
  | { kind: 'launch_app'; app_id: string; name: string }
  | { kind: 'hotkey'; keys: string[] }
  | { kind: 'key_sequence'; steps?: string[][]; events?: KeySequenceEvent[] }
  | { kind: 'open_url'; url: string }

export interface Gesture {
  id: string
  name: string
  enabled: boolean
  deletable: boolean
  /** null = 不按键，仅靠边角触发 */
  trigger: TriggerButton | null
  region: Region | null
  wheel: WheelDir[]
  left_click: boolean
  /** 大于 0 表示边角停留手势：鼠标在 region 停留这么久（ms）后触发 */
  dwell_ms: number
  fire_mode: FireMode
  points: Point[]
  times: number[]
  /** 键盘快捷键，例如 ["Ctrl","Alt","A"]；空或缺省表示没有 */
  hotkey?: string[]
  action: Action
}

export interface Profile {
  id: string
  name: string
  exe: string
  /** true = 交给程序自带手势：NextGestures 在该程序里不拦截鼠标、不响应快捷键。Global 永远为 false */
  passthrough?: boolean
  gestures: Gesture[]
}

export interface Settings {
  trigger_buttons: TriggerButton[]
  move_threshold: number
  match_threshold: number
  show_trail: boolean
  quiet_mode: boolean
  trail_color: string
  trail_width: number
  glow_color: string
  glow_blur: number
  miss_color: string
  gesture_hint_font_size: number
  gesture_hint_color: string
  gesture_hint_offset_x: number
  gesture_hint_offset_y: number
  gesture_hint_topmost: boolean
  toggle_hotkey: string[]
  language: string
  api_base: string
  feedback_nickname: string
  ai: AiSettings
}

export interface Config {
  version: number
  settings: Settings
  profiles: Profile[]
}

export interface AiAction {
  id: string
  name: string
  prompt: string
  mode: 'bubble' | 'replace'
  /** lucide 图标名（PascalCase），空表示默认 */
  icon?: string
  /** 把回复中单独一行 <notes> 之后的内容作为说明卡片显示（替换时不粘贴） */
  notes?: boolean
}

export interface AiSettings {
  actions: AiAction[]
  base_url: string
  api_key: string
  model: string
  /** 预设已写入过「我的动作」，删掉的预设不会自动补回 */
  presets_seeded: boolean
}

export interface RecordedPayload {
  trigger: TriggerButton | null
  region: Region | null
  wheel: WheelDir[]
  left_click: boolean
  points: Point[]
  times: number[]
  raw: Point[]
}

export interface LivePayload {
  trigger: TriggerButton | null
  region: Region | null
  wheel: WheelDir[]
  left_click: boolean
  points: Point[]
}

export interface FiredPayload {
  profile_id: string
  gesture_id: string
  name: string
  score: number
  exe: string
}

export const api = {
  getConfig: () => invoke<Config>('get_config'),
  configPath: () => invoke<string>('config_path'),
  saveSettings: (settings: Settings) => invoke<void>('save_settings', { settings }),
  upsertGesture: (profileId: string, gesture: Gesture) =>
    invoke<void>('upsert_gesture', { profileId, gesture }),
  removeGesture: (profileId: string, gestureId: string) =>
    invoke<void>('remove_gesture', { profileId, gestureId }),
  setGestureEnabled: (profileId: string, gestureId: string, enabled: boolean) =>
    invoke<void>('set_gesture_enabled', { profileId, gestureId, enabled }),
  upsertProfile: (profile: Profile) => invoke<void>('upsert_profile', { profile }),
  removeProfile: (profileId: string) => invoke<void>('remove_profile', { profileId }),
  setProfilePassthrough: (profileId: string, passthrough: boolean) =>
    invoke<void>('set_profile_passthrough', { profileId, passthrough }),
  /** 因被其他程序占用而注册失败的快捷键标签，如 "Ctrl + Alt + T" */
  hotkeyFailures: () => invoke<string[]>('hotkey_failures'),
  startRecording: () => invoke<void>('start_recording'),
  cancelRecording: () => invoke<void>('cancel_recording'),
  beginPick: () => invoke<void>('begin_pick'),
  cancelPick: () => invoke<void>('cancel_pick'),
  /** 结果通过 `hotkey:mods` / `hotkey:recorded` 事件回报 */
  beginKeyRecording: () => invoke<void>('begin_key_recording'),
  cancelKeyRecording: () => invoke<void>('cancel_key_recording'),
  beginKeySequence: (id: string) => invoke<void>('begin_key_sequence', { id }),
  stopKeySequence: (id: string) => invoke<KeySequenceEvent[]>('stop_key_sequence', { id }),
  cancelKeySequence: (id: string) => invoke<void>('cancel_key_sequence', { id }),
  setEnabled: (enabled: boolean) => invoke<void>('set_enabled', { enabled }),
  isEnabled: () => invoke<boolean>('is_enabled'),
  foregroundExe: () => invoke<string>('foreground_exe'),
  virtualScreenSize: () => invoke<VirtualScreenSize>('virtual_screen_size'),
  testAction: (action: Action) => invoke<void>('test_action', { action }),
  listInstalledApps: () => invoke<InstalledApp[]>('list_installed_apps'),
  showGestureHintPreview: (hint: GestureHintPreview) =>
    invoke<void>('show_gesture_hint_preview', { hint }),
  hideGestureHintPreview: () => invoke<void>('hide_gesture_hint_preview'),
  hideSettings: () => invoke<void>('hide_settings'),
  isAutostartEnabled: () => invoke<boolean>('is_autostart_enabled'),
  setAutostartEnabled: (enabled: boolean) => invoke<void>('set_autostart_enabled', { enabled }),
  feedbackChat: () => invoke<ChatItem[]>('feedback_chat'),
  /** 返回服务端 ack 文案 */
  feedbackSubmit: (name: string, message: string) =>
    invoke<string>('feedback_submit', { name, message }),
  feedbackAckUnread: () => invoke<void>('feedback_ack_unread'),
}

export interface InstalledApp {
  id: string
  name: string
  path: string
  icon: string | null
}

export interface ChatItem {
  type: 'user' | 'admin'
  id: number
  content: string
  /** UTC，ISO 8601，无时区后缀 */
  created_at: string
  user_name: string
  status: string
  is_read: boolean
  via: string
}

export interface GestureHintPreview {
  font_size: number
  color: string
  offset_x: number
  offset_y: number
}

export interface VirtualScreenSize {
  width: number
  height: number
}

export const TRIGGERS: TriggerButton[] = ['Right', 'Middle', 'X1', 'X2']
export const REGIONS: Region[] = [
  'TopLeft',
  'Top',
  'TopRight',
  'Right',
  'BottomRight',
  'Bottom',
  'BottomLeft',
  'Left',
]

export function newId(): string {
  return crypto.randomUUID()
}

export function emptyGesture(trigger: TriggerButton = 'Right'): Gesture {
  return {
    id: newId(),
    name: '',
    enabled: true,
    deletable: true,
    trigger,
    region: null,
    wheel: [],
    left_click: false,
    dwell_ms: 0,
    fire_mode: 'OnRelease',
    points: [],
    times: [],
    hotkey: [],
    action: { kind: 'none' },
  }
}

/** 以已有手势为模板复制出一个新手势草稿：新 id、可删除、清空快捷键，其余数据（轨迹、触发、动作）原样保留 */
export function cloneGesture(src: Gesture, suffix: string): Gesture {
  return {
    ...structuredClone(src),
    // 同一配置组内快捷键不能重复，副本不带快捷键
    hotkey: [],
    id: newId(),
    name: src.name ? `${src.name} ${suffix}` : '',
    enabled: true,
    deletable: true,
  }
}

export const DOCS_URL = 'https://doerrdev.github.io/NG-Doc/'
export const DOCS_GESTURE_URL = `${DOCS_URL}guide/triggers/gesture.html`

/** 在系统浏览器打开外链；非 Tauri 环境（vite 预览）退回 window.open */
export async function openExternal(url: string): Promise<void> {
  if (!isTauri()) {
    window.open(url, '_blank', 'noopener,noreferrer')
    return
  }
  await api.testAction({ kind: 'open_url', url })
}

/** 快捷键显示文本，例如 "Ctrl + Alt + A"；与后端 hotkey_failures 返回的格式一致 */
export function hotkeyLabel(keys: string[] | undefined): string {
  return (keys ?? []).join(' + ')
}

export function hasHotkey(g: Gesture): boolean {
  return (g.hotkey?.length ?? 0) > 0
}

/** 是否配置了鼠标触发（轨迹、滚轮或屏幕边角） */
export function hasMouseTrigger(g: Gesture): boolean {
  return g.points.length > 0 || g.wheel.length > 0 || g.region != null
}

/** 只有快捷键、没有鼠标触发 */
export function isHotkeyOnly(g: Gesture): boolean {
  return hasHotkey(g) && !hasMouseTrigger(g)
}
