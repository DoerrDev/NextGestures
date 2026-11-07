import { convertFileSrc, invoke } from '@tauri-apps/api/core'

export type ScreenTool = 'screenshot' | 'pin' | 'pip' | 'ocr'
export interface Rect { x: number; y: number; width: number; height: number }
export interface ScreenSession { label: string; kind: string; tool: ScreenTool; bounds: Rect; click_through: boolean }
export interface ScreenOutput { text: string | null; path: string | null; label: string | null; canceled: boolean }
export interface OcrLanguage { tag: string; name: string }
export const TOOL_NAMES: Record<ScreenTool, string> = { screenshot: '截图标注', pin: '贴图置顶', pip: '画中画', ocr: '文字识别' }
export const screens = {
  start: (tool: ScreenTool) => invoke<string>('start_screen_tool', { tool }),
  session: () => invoke<ScreenSession>('screen_session'),
  windows: () => invoke<ScreenSession[]>('screen_windows'),
  image: (label: string) => convertFileSrc(`${label}.png`, 'ngimage'),
  output: (kind: string, data = '', rect: Rect | null = null, language = '') => invoke<ScreenOutput>('screen_output', { kind, data, rect, language }),
  languages: () => invoke<OcrLanguage[]>('screen_ocr_languages'),
  copyText: (text: string) => invoke<void>('screen_copy_text', { text }),
  control: (label: string, operation: string, value?: number) => invoke<void>('screen_control', { label, operation, value }),
}
