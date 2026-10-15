import { invoke } from '@tauri-apps/api/core'

const isTauri = () => typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window

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
  if (isTauri()) {
    await invoke('plugin:opener|open_url', { url: parsed.href })
    return
  }
  window.open(parsed.href, '_blank', 'noopener,noreferrer')
}

/**
 * 打开 `ccswitch://` 一键导入链接，交给本机已安装的 CC Switch 处理（它会弹出确认框，用户确认后才导入）。
 * 只放行这一种协议；Tauri 侧另由 capability 的 opener:allow-open-url 作用域限定为 `ccswitch://*`。
 */
export async function openCcSwitchLink(url: string): Promise<void> {
  if (!url.startsWith('ccswitch://')) throw new Error('只允许打开 ccswitch:// 链接')
  if (isTauri()) {
    await invoke('plugin:opener|open_url', { url })
    return
  }
  window.location.href = url
}
