import type { UsageRange } from '@/types'

export const RANGE_OPTIONS: { value: UsageRange; label: string }[] = [
  { value: 'today', label: '今天' },
  { value: '24h', label: '24小时' },
  { value: '7d', label: '7天' },
  { value: '30d', label: '30天' },
  { value: 'all', label: '全部' },
]

/** 与上一周期对比的文案；'all' 没有可比周期 */
export function compareLabel(range: UsageRange): string {
  switch (range) {
    case 'today':
      return '较昨日'
    case '24h':
      return '较前 24 小时'
    case '7d':
      return '较前 7 天'
    case '30d':
      return '较前 30 天'
    default:
      return ''
  }
}

const intFmt = new Intl.NumberFormat('en-US')

export function formatInt(n: number): string {
  return intFmt.format(Math.round(n))
}

/** 1234 -> 1.2K，用于图表坐标轴与排行榜 */
export function formatCompact(n: number): string {
  const abs = Math.abs(n)
  if (abs >= 1e9) return `${trim(n / 1e9)}B`
  if (abs >= 1e6) return `${trim(n / 1e6)}M`
  if (abs >= 1e3) return `${trim(n / 1e3)}K`
  return trim(n)
}

function trim(n: number): string {
  return Number.isInteger(n) ? String(n) : n.toFixed(1).replace(/\.0$/, '')
}

/** 成本：极小金额保留 4 位小数，避免显示成 $0.00 造成“免费”的错觉 */
export function formatCost(n: number): string {
  if (n === 0) return '$0.00'
  if (Math.abs(n) < 0.01) return `$${n.toFixed(4)}`
  return `$${n.toFixed(2)}`
}

export function formatPct(pct: number): string {
  return `${pct.toFixed(1)}%`
}

export function formatDelta(deltaPct: number): string {
  const sign = deltaPct > 0 ? '+' : ''
  return `${sign}${Math.round(deltaPct)}%`
}

export function formatLatency(ms: number): string {
  return ms >= 1000 ? `${(ms / 1000).toFixed(1)}s` : `${Math.round(ms)}ms`
}

/** 相对时间，例如“5 分钟前”；无法解析返回 “—” */
export function formatRelative(iso: string | null, now = Date.now()): string {
  if (!iso) return '—'
  const t = Date.parse(iso)
  if (Number.isNaN(t)) return '—'
  const sec = Math.max(0, Math.round((now - t) / 1000))
  if (sec < 45) return '刚刚'
  const min = Math.round(sec / 60)
  if (min < 60) return `${min} 分钟前`
  const hour = Math.round(min / 60)
  if (hour < 24) return `${hour} 小时前`
  const day = Math.round(hour / 24)
  return `${day} 天前`
}

function pad(n: number): string {
  return String(n).padStart(2, '0')
}

/** 本地时间 MM-DD HH:mm:ss */
export function formatDateTime(iso: string): string {
  const d = new Date(iso)
  if (Number.isNaN(d.getTime())) return '—'
  return `${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`
}

const PALETTE = ['#3b82f6', '#16a36b', '#f4a524', '#8b5cf6', '#06b6d4', '#ec4899', '#84cc16', '#f97316']

/** 同一 id 永远得到同一个纯色（无渐变） */
export function colorFor(key: string): string {
  let h = 0
  for (let i = 0; i < key.length; i++) h = (h * 31 + key.charCodeAt(i)) >>> 0
  return PALETTE[h % PALETTE.length]!
}
