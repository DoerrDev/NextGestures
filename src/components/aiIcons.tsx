import {
  Archive, AtSign, Bell, BookOpen, Bot, Brain, Briefcase, Bug, Calculator, Calendar, ChartBar, Check, CircleHelp,
  ClipboardList, Code, Coffee, Compass, Database, Eraser, FileText, Filter, Flag, Folder, Gift, Globe, GraduationCap,
  Hash, Heart, House, Image, Info, Key, Languages, Lightbulb, Link, ListChecks, Lock, Mail, Megaphone,
  MessageCircle, MessageSquare, Mic, Music, Newspaper, Palette, Pencil, PenLine, Quote, Rocket, Scale, Scissors,
  Search, Send, Shield, ShoppingCart, Smile, Sparkles, SpellCheck, Star, Sticker, Table, Tag, Target, Terminal,
  ThumbsUp, Timer, Trophy, Type, User, Users, Volume2, WandSparkles, Wrench, Zap,
  type LucideIcon,
} from 'lucide-react'

/** 自定义 AI 动作的默认图标 */
export const DEFAULT_AI_ICON = 'Sparkles'

/** 可选图标集：名字是 lucide 的 PascalCase 名，保存到配置里 */
const ICONS: Record<string, LucideIcon> = {
  Archive, AtSign, Bell, BookOpen, Bot, Brain, Briefcase, Bug, Calculator, Calendar, ChartBar, Check, CircleHelp,
  ClipboardList, Code, Coffee, Compass, Database, Eraser, FileText, Filter, Flag, Folder, Gift, Globe, GraduationCap,
  Hash, Heart, House, Image, Info, Key, Languages, Lightbulb, Link, ListChecks, Lock, Mail, Megaphone,
  MessageCircle, MessageSquare, Mic, Music, Newspaper, Palette, Pencil, PenLine, Quote, Rocket, Scale, Scissors,
  Search, Send, Shield, ShoppingCart, Smile, Sparkles, SpellCheck, Star, Sticker, Table, Tag, Target, Terminal,
  ThumbsUp, Timer, Trophy, Type, User, Users, Volume2, WandSparkles, Wrench, Zap,
}

/** 图标选择器里直接露出的常用图标 */
export const COMMON_AI_ICONS = [
  'Sparkles', 'Languages', 'WandSparkles', 'Mail', 'Bug', 'BookOpen',
  'MessageSquare', 'FileText', 'Code', 'Brain', 'Lightbulb', 'ListChecks',
]

export const ALL_AI_ICON_NAMES = Object.keys(ICONS)

/** 按名字取图标；名字为空或不在图标集里时回落到默认图标 */
export function aiIcon(name?: string): LucideIcon {
  return (name && ICONS[name]) || Sparkles
}
