import type { ProxyNode, ProxyNodeStatus, ProxyProviderScope, ProxyScheme, ProxyStrategy } from '@/types'

type StatusTone = 'success' | 'warning' | 'danger' | 'neutral' | 'brand'

export const STRATEGY_OPTIONS: { id: ProxyStrategy; label: string; description: string }[] = [
  { id: 'roundRobin', label: '轮询', description: '依次轮流使用每个可用节点，把请求均摊到各个出口。' },
  { id: 'random', label: '随机', description: '每次随机挑一个可用节点作为首选。' },
  { id: 'failover', label: '主备', description: '始终优先使用列表里靠前的可用节点，它失败或冷却时才用后面的。' },
  { id: 'lowestLatency', label: '最低延迟', description: '按最近一次测速由快到慢选择，未测速的节点排在最后。请先点“全部测速”。' },
]

export const SCOPE_OPTIONS: { id: ProxyProviderScope; label: string }[] = [
  { id: 'all', label: '所有提供商' },
  { id: 'include', label: '仅指定的提供商' },
  { id: 'exclude', label: '除指定的提供商外' },
]

export const SCHEME_OPTIONS: { id: ProxyScheme; label: string }[] = [
  { id: 'http', label: 'HTTP' },
  { id: 'https', label: 'HTTPS' },
  { id: 'socks5', label: 'SOCKS5' },
  { id: 'socks5h', label: 'SOCKS5H（由代理解析域名）' },
]

export function strategyLabel(id: ProxyStrategy): string {
  return STRATEGY_OPTIONS.find((o) => o.id === id)?.label ?? id
}

const STATUS: Record<ProxyNodeStatus, { label: string; tone: StatusTone }> = {
  untested: { label: '未测速', tone: 'neutral' },
  healthy: { label: '正常', tone: 'success' },
  unhealthy: { label: '异常', tone: 'danger' },
  cooldown: { label: '冷却中', tone: 'warning' },
}

export function statusOf(node: ProxyNode): { label: string; tone: StatusTone } {
  if (!node.enabled) return { label: '已停用', tone: 'neutral' }
  return STATUS[node.status]
}

/** 展示用地址，不含账号密码 */
export function nodeAddress(n: Pick<ProxyNode, 'scheme' | 'host' | 'port'>): string {
  const host = n.host.includes(':') ? `[${n.host}]` : n.host
  return `${n.scheme}://${host}:${n.port}`
}
