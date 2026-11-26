import { icons, Sparkles, type LucideIcon } from 'lucide-react'

/** 自定义 AI 动作的默认图标 */
export const DEFAULT_AI_ICON = 'Sparkles'

/** 图标选择器里直接露出的常用图标（lucide 的 PascalCase 名） */
export const COMMON_AI_ICONS = [
  'Sparkles', 'Languages', 'WandSparkles', 'Mail', 'Bug', 'BookOpen',
  'MessageSquare', 'FileText', 'Code', 'Brain', 'Lightbulb', 'ListChecks',
]

export const ALL_AI_ICON_NAMES = Object.keys(icons)

/** 按名字取 lucide 图标；名字为空或已不存在时回落到默认图标 */
export function aiIcon(name?: string): LucideIcon {
  return (name && (icons as Record<string, LucideIcon>)[name]) || Sparkles
}
