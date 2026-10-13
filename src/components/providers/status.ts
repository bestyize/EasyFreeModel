import type { Connection, ProviderSummary, Protocol } from '@/types'
import type { StatusTone } from '@/components/StatusTag.vue'

export const PROTOCOL_LABEL: Record<Protocol, string> = {
  'openai-chat': 'OpenAI Chat',
  'openai-responses': 'OpenAI Responses',
  'anthropic-messages': 'Anthropic Messages',
  gemini: 'Gemini',
}

export function protocolText(protocols: Protocol[]): string {
  return protocols.length ? protocols.map((p) => PROTOCOL_LABEL[p]).join(' / ') : '协议待定'
}

/**
 * 提供商状态文案。适配器未接入时一律显示“待接入”，
 * 无论后端给出什么 status，都不会显示为“已连接”。
 */
export function providerStatus(p: Pick<ProviderSummary, 'adapterReady' | 'status'>): { text: string; tone: StatusTone } {
  if (!p.adapterReady) return { text: '待接入', tone: 'neutral' }
  switch (p.status) {
    case 'connected':
      return { text: '已连接', tone: 'success' }
    case 'error':
      return { text: '异常', tone: 'danger' }
    case 'checking':
      return { text: '检测中', tone: 'warning' }
    case 'unsupported':
      return { text: '暂不支持', tone: 'neutral' }
    default:
      return { text: '未连接', tone: 'neutral' }
  }
}

export function connectionStatus(c: Pick<Connection, 'status' | 'enabled'>, pendingAuth = false): { text: string; tone: StatusTone } {
  if (pendingAuth) return { text: '待授权', tone: 'warning' }
  // 令牌已失效的 OAuth 连接不参与路由，无论是否启用都不能显示成“活跃”
  if (c.status === 'needsReauth') return { text: '需要重新授权', tone: 'warning' }
  if (!c.enabled) return { text: '已停用', tone: 'neutral' }
  if (c.status === 'error') return { text: '异常', tone: 'danger' }
  if (c.status === 'paused') return { text: '已暂停', tone: 'neutral' }
  return { text: '活跃', tone: 'success' }
}

/** 首字母 Logo 的纯色板（无渐变） */
const LOGO_COLORS = ['#ff2442', '#2f6bff', '#16a36b', '#f4a524', '#7a4dff', '#0e9aa7', '#e4572e', '#3d4351']

export function logoColor(name: string): string {
  let h = 0
  for (let i = 0; i < name.length; i++) h = (h * 31 + name.charCodeAt(i)) >>> 0
  return LOGO_COLORS[h % LOGO_COLORS.length]
}

export function logoLetter(name: string): string {
  const ch = name.trim().charAt(0)
  return ch ? ch.toUpperCase() : '?'
}

/** 目录文字图标优先（最多 3 个字符），否则退回名称首字母 */
export function logoText(name: string, textIcon?: string | null): string {
  const t = textIcon?.trim()
  return t ? Array.from(t).slice(0, 3).join('') : logoLetter(name)
}

/** 品牌色块上的前景色：浅底用深色字，深底用白字（按相对亮度，不使用渐变） */
export function readableOn(hex: string): string {
  const m = /^#([0-9a-f]{6})$/i.exec(hex)
  if (!m) return '#ffffff'
  const n = parseInt(m[1], 16)
  const [r, g, b] = [(n >> 16) & 255, (n >> 8) & 255, n & 255]
  const lum = (0.2126 * r + 0.7152 * g + 0.0722 * b) / 255
  return lum > 0.72 ? '#16181d' : '#ffffff'
}

export function isLightColor(hex: string): boolean {
  return readableOn(hex) !== '#ffffff'
}

/** 能力筛选：把目录的原始 serviceKinds 归并成界面上的六类 */
export const CAPABILITIES = [
  { id: 'chat', label: '聊天', kinds: ['llm'] },
  { id: 'image', label: '图像', kinds: ['image', 'imageToText'] },
  { id: 'voice', label: '语音', kinds: ['tts', 'stt'] },
  { id: 'video', label: '视频', kinds: ['video'] },
  { id: 'search', label: '搜索', kinds: ['webSearch', 'webFetch'] },
  { id: 'embedding', label: '嵌入', kinds: ['embedding'] },
] as const

export type CapabilityId = (typeof CAPABILITIES)[number]['id']

export function hasCapability(serviceKinds: readonly string[], id: CapabilityId): boolean {
  const cap = CAPABILITIES.find((c) => c.id === id)
  return !!cap && serviceKinds.some((k) => (cap.kinds as readonly string[]).includes(k))
}

/** 卡片上的能力小标签（按固定顺序、去重） */
export function capabilityLabels(serviceKinds: readonly string[]): string[] {
  return CAPABILITIES.filter((c) => hasCapability(serviceKinds, c.id)).map((c) => c.label)
}
