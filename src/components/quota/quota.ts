import type { QuotaAccount, QuotaPeriod } from '@/types'

/**
 * 剩余额度颜色阈值（百分比）。PRODUCT_DESIGN §4.4：
 * 剩余 > warn 为绿；danger < 剩余 <= warn 为黄；剩余 <= danger 为红。可配置：改这里即可。
 */
export interface QuotaThresholds {
  warn: number
  danger: number
}
export const QUOTA_WARN_PCT = 20
export const QUOTA_DANGER_PCT = 5
export const DEFAULT_THRESHOLDS: QuotaThresholds = { warn: QUOTA_WARN_PCT, danger: QUOTA_DANGER_PCT }

/** 周期很多时卡片默认只展示前几项，其余折叠 */
export const COLLAPSED_PERIODS = 5

export type QuotaLevel = 'ok' | 'warn' | 'danger' | 'unknown'

const round6 = (n: number) => Math.round(n * 1e6) / 1e6

/**
 * 剩余百分比（0–100）。无法确定时返回 null —— null 代表“未知”，绝不能当作 0。
 * 优先用 remaining/total；没有 remaining 时用 (total-used)/total。
 */
export function remainingPct(p: Pick<QuotaPeriod, 'used' | 'total' | 'remaining'>): number | null {
  const { total, remaining, used } = p
  if (total === null || !Number.isFinite(total) || total <= 0) return null
  let pct: number | null = null
  if (remaining !== null && Number.isFinite(remaining)) pct = (remaining / total) * 100
  else if (used !== null && Number.isFinite(used)) pct = ((total - used) / total) * 100
  if (pct === null) return null
  // 先取 6 位小数，避免 0.2*100 之类的浮点误差让 20% 边界判错
  return Math.min(100, Math.max(0, round6(pct)))
}

export function quotaLevel(pct: number | null, t: QuotaThresholds = DEFAULT_THRESHOLDS): QuotaLevel {
  if (pct === null) return 'unknown'
  if (pct <= t.danger) return 'danger'
  if (pct <= t.warn) return 'warn'
  return 'ok'
}

/** 剩余百分比文案：未知 -> “未知”；0 -> “0%”；不把 0.4% 四舍五入成 0%，也不把 99.6% 显示成 100% */
export function formatRemainingPct(pct: number | null): string {
  if (pct === null) return '未知'
  if (pct === 0) return '0%'
  if (pct < 1) return '<1%'
  const r = Math.round(pct)
  return `${r === 100 && pct < 100 ? 99 : r}%`
}

const numFmt = new Intl.NumberFormat('en-US', { maximumFractionDigits: 2 })

/** null -> “—”（未知），0 -> “0” */
export function formatAmount(n: number | null): string {
  return n === null || !Number.isFinite(n) ? '—' : numFmt.format(n)
}

export function formatBalance(amount: number | null, currency: string): string {
  if (amount === null || !Number.isFinite(amount)) return '未知'
  return `${amount.toFixed(2)} ${currency}`
}

/** 已用/总量 文案，缺失的部分以“—”表示 */
export function usageText(p: Pick<QuotaPeriod, 'used' | 'total' | 'remaining'>): string {
  const { used, total, remaining } = p
  if (used !== null && total !== null) return `${formatAmount(used)} / ${formatAmount(total)}`
  if (remaining !== null && total !== null) return `剩余 ${formatAmount(remaining)} / ${formatAmount(total)}`
  if (remaining !== null) return `剩余 ${formatAmount(remaining)} / —`
  return `${formatAmount(used)} / ${formatAmount(total)}`
}

/** 距离重置的倒计时；未知返回 null；已过重置时间返回“待重置” */
export function formatCountdown(resetsAt: string | null, now: number): string | null {
  if (!resetsAt) return null
  const t = Date.parse(resetsAt)
  if (Number.isNaN(t)) return null
  const diff = t - now
  if (diff <= 0) return '待重置'
  const totalMin = Math.floor(diff / 60_000)
  const d = Math.floor(totalMin / 1440)
  const h = Math.floor((totalMin % 1440) / 60)
  const m = totalMin % 60
  if (d > 0) return `${d}d ${h}h ${m}m`
  if (h > 0) return `${h}h ${m}m`
  return `${m}m`
}

export function sourceLabel(source: QuotaPeriod['source']): string | null {
  if (source === 'estimated') return '估算'
  if (source === 'cached') return '缓存'
  return null
}

export function formatSyncedAt(iso: string | null, now: number): string {
  if (!iso) return '尚未同步'
  const t = Date.parse(iso)
  if (Number.isNaN(t)) return '尚未同步'
  const sec = Math.max(0, Math.round((now - t) / 1000))
  if (sec < 45) return '刚刚'
  const min = Math.round(sec / 60)
  if (min < 60) return `${min} 分钟前`
  const hour = Math.round(min / 60)
  if (hour < 24) return `${hour} 小时前`
  return `${Math.round(hour / 24)} 天前`
}

/** 账户的所有“额度信号”：每个周期的剩余百分比（可为 null）+ 每个余额是否 >0（null = 未知） */
function signals(a: QuotaAccount): { known: boolean; positive: boolean }[] {
  return [
    ...a.periods.map((p) => {
      const pct = remainingPct(p)
      return { known: pct !== null, positive: pct !== null && pct > 0 }
    }),
    ...a.balances.map((b) => ({ known: b.amount !== null, positive: b.amount !== null && b.amount > 0 })),
  ]
}

/** 空账号：至少有一项额度数据，且全部“已知且为 0”。含未知项时保守地视为非空，避免误关。 */
export function isEmptyAccount(a: QuotaAccount): boolean {
  const s = signals(a)
  return s.length > 0 && s.every((x) => x.known && !x.positive)
}

/** 可用账号：没有同步错误，且至少有一项已知的正额度 */
export function isAvailableAccount(a: QuotaAccount): boolean {
  return a.error === null && signals(a).some((x) => x.positive)
}

/** 最近的未来重置时间（毫秒时间戳）；没有则 Infinity，排序时靠后 */
export function soonestReset(a: QuotaAccount, now: number): number {
  let best = Infinity
  for (const p of a.periods) {
    if (!p.resetsAt) continue
    const t = Date.parse(p.resetsAt)
    if (!Number.isNaN(t) && t > now && t < best) best = t
  }
  return best
}
