import { listen } from '@tauri-apps/api/event'

/**
 * 主窗口是否真的在屏幕上。
 * 关到托盘只是 ShowWindow(SW_HIDE)，WebView2 不会因此把 document 标成 hidden，
 * requestAnimationFrame 照跑，所以由后端在隐藏/显示/最小化时显式通知。
 */
let windowVisible = true
const subs = new Set<() => void>()

export function isPageActive(): boolean {
  return windowVisible && document.visibilityState === 'visible'
}

/** 状态变化时回调，返回取消订阅函数 */
export function onPageActiveChange(fn: () => void): () => void {
  subs.add(fn)
  return () => subs.delete(fn)
}

const notify = () => subs.forEach((fn) => fn())

document.addEventListener('visibilitychange', notify)
void listen<boolean>('main:visibility', (e) => {
  if (windowVisible === e.payload) return
  windowVisible = e.payload
  notify()
})
