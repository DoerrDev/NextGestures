import { invoke } from '@tauri-apps/api/core'

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

export type Action =
  | { kind: 'none' }
  | { kind: 'command'; program: string; args: string[]; shell: boolean }
  | { kind: 'hotkey'; keys: string[] }
  | { kind: 'key_sequence'; steps: string[][] }
  | { kind: 'open_url'; url: string }
  | { kind: 'lua'; code: string }

export interface Gesture {
  id: string
  name: string
  enabled: boolean
  deletable: boolean
  /** null = 不按键，仅靠边角触发 */
  trigger: TriggerButton | null
  region: Region | null
  wheel: WheelDir[]
  fire_mode: FireMode
  points: Point[]
  times: number[]
  action: Action
}

export interface Profile {
  id: string
  name: string
  exe: string
  gestures: Gesture[]
}

export interface Settings {
  trigger_buttons: TriggerButton[]
  move_threshold: number
  match_threshold: number
  show_trail: boolean
  trail_color: string
  trail_width: number
  glow_color: string
  glow_blur: number
  miss_color: string
  gesture_hint_font_size: number
  gesture_hint_color: string
  gesture_hint_offset_x: number
  gesture_hint_offset_y: number
  toggle_hotkey: string[]
  language: string
  api_base: string
  feedback_nickname: string
}

export interface Config {
  version: number
  settings: Settings
  profiles: Profile[]
}

export interface RecordedPayload {
  trigger: TriggerButton | null
  region: Region | null
  wheel: WheelDir[]
  points: Point[]
  times: number[]
  raw: Point[]
}

export interface LivePayload {
  trigger: TriggerButton | null
  region: Region | null
  wheel: WheelDir[]
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
  startRecording: () => invoke<void>('start_recording'),
  cancelRecording: () => invoke<void>('cancel_recording'),
  beginPick: () => invoke<void>('begin_pick'),
  setEnabled: (enabled: boolean) => invoke<void>('set_enabled', { enabled }),
  isEnabled: () => invoke<boolean>('is_enabled'),
  foregroundExe: () => invoke<string>('foreground_exe'),
  virtualScreenSize: () => invoke<VirtualScreenSize>('virtual_screen_size'),
  testAction: (action: Action) => invoke<void>('test_action', { action }),
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
    fire_mode: 'OnRelease',
    points: [],
    times: [],
    action: { kind: 'none' },
  }
}
