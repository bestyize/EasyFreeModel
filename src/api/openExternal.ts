import { invoke } from '@tauri-apps/api/core'

/**
 * 在系统浏览器中打开外部链接（OAuth 授权页、设备码验证页）。
 * 只允许 http / https；不内嵌 WebView 登录。
 * Tauri 下走 opener 插件命令（Rust 侧已注册，capability 为 opener:default）；
 * 浏览器预览下用 window.open，并带 noopener,noreferrer。
 */
export async function openExternal(url: string): Promise<void> {
  let parsed: URL
  try {
    parsed = new URL(url)
  } catch {
    throw new Error('无效的链接')
  }
  if (parsed.protocol !== 'http:' && parsed.protocol !== 'https:') {
    throw new Error('只允许打开 http / https 链接')
  }
  if (typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window) {
    await invoke('plugin:opener|open_url', { url: parsed.href })
    return
  }
  window.open(parsed.href, '_blank', 'noopener,noreferrer')
}
