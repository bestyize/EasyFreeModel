import { describe, expect, it } from 'vitest'
import type { QuotaAccount, QuotaPeriod } from '@/types'
import {
  QUOTA_DANGER_PCT,
  QUOTA_WARN_PCT,
  formatAmount,
  formatBalance,
  formatCountdown,
  formatRemainingPct,
  isAvailableAccount,
  isEmptyAccount,
  quotaLevel,
  remainingPct,
  soonestReset,
  usageText,
} from './quota'

const period = (over: Partial<QuotaPeriod> = {}): QuotaPeriod => ({
  name: '5h',
  unit: 'requests',
  used: 0,
  total: 100,
  remaining: 100,
  resetsAt: null,
  source: 'live',
  ...over,
})

const acct = (over: Partial<QuotaAccount> = {}): QuotaAccount => ({
  connectionId: 'c1',
  providerId: 'p',
  providerName: 'P',
  accountLabel: 'a',
  enabled: true,
  periods: [],
  balances: [],
  lastSyncedAt: null,
  error: null,
  ...over,
})

describe('颜色阈值', () => {
  it('阈值常量为 20 / 5', () => {
    expect(QUOTA_WARN_PCT).toBe(20)
    expect(QUOTA_DANGER_PCT).toBe(5)
  })

  it('>20 绿；20 与 5<x<=20 黄；<=5 红', () => {
    expect(quotaLevel(100)).toBe('ok')
    expect(quotaLevel(20.01)).toBe('ok')
    expect(quotaLevel(20)).toBe('warn')
    expect(quotaLevel(19.99)).toBe('warn')
    expect(quotaLevel(5.01)).toBe('warn')
    expect(quotaLevel(5)).toBe('danger')
    expect(quotaLevel(4.99)).toBe('danger')
    expect(quotaLevel(0)).toBe('danger')
  })

  it('可通过参数配置阈值', () => {
    expect(quotaLevel(30, { warn: 50, danger: 10 })).toBe('warn')
    expect(quotaLevel(10, { warn: 50, danger: 10 })).toBe('danger')
  })

  it('浮点边界：remaining=20,total=100 与 remaining=0.2,total=1 都恰为 20% -> 黄', () => {
    expect(quotaLevel(remainingPct(period({ remaining: 20, total: 100 })))).toBe('warn')
    expect(quotaLevel(remainingPct(period({ remaining: 0.2, total: 1 })))).toBe('warn')
    expect(quotaLevel(remainingPct(period({ remaining: 0.05, total: 1 })))).toBe('danger')
  })
})

describe('0 与未知的区别', () => {
  it('未知（null）-> pct=null、level=unknown、文案“未知”，不是 0%', () => {
    const unknown = period({ used: null, total: null, remaining: null })
    expect(remainingPct(unknown)).toBeNull()
    expect(quotaLevel(remainingPct(unknown))).toBe('unknown')
    expect(formatRemainingPct(null)).toBe('未知')
    expect(formatRemainingPct(null)).not.toContain('0')
  })

  it('已知为 0（remaining=0）-> pct=0、level=danger、文案“0%”', () => {
    const zero = period({ used: 100, total: 100, remaining: 0 })
    expect(remainingPct(zero)).toBe(0)
    expect(quotaLevel(0)).toBe('danger')
    expect(formatRemainingPct(0)).toBe('0%')
  })

  it('total 已知但 remaining/used 都未知 -> 未知，不是 0 也不是 100', () => {
    expect(remainingPct(period({ used: null, remaining: null, total: 50 }))).toBeNull()
  })

  it('total 为 null 或 0 -> 无法计算百分比', () => {
    expect(remainingPct(period({ total: null, remaining: 10 }))).toBeNull()
    expect(remainingPct(period({ total: 0, used: 0, remaining: 0 }))).toBeNull()
  })

  it('只有 used+total 时用 (total-used)/total', () => {
    expect(remainingPct(period({ used: 346, total: 1000, remaining: null }))).toBe(65.4)
  })

  it('越界值被夹到 0–100', () => {
    expect(remainingPct(period({ used: null, remaining: 150, total: 100 }))).toBe(100)
    expect(remainingPct(period({ used: null, remaining: -5, total: 100 }))).toBe(0)
  })

  it('数值/金额格式：null -> “—”/“未知”，0 -> “0”/“0.00 USD”', () => {
    expect(formatAmount(null)).toBe('—')
    expect(formatAmount(0)).toBe('0')
    expect(formatAmount(1000)).toBe('1,000')
    expect(formatBalance(null, 'USD')).toBe('未知')
    expect(formatBalance(0, 'USD')).toBe('0.00 USD')
    expect(formatBalance(55.7, 'CNY')).toBe('55.70 CNY')
  })

  it('已用/总量文案：缺失部分用 “—”', () => {
    expect(usageText(period({ used: 346, total: 1000 }))).toBe('346 / 1,000')
    expect(usageText(period({ used: null, total: null, remaining: null }))).toBe('— / —')
    expect(usageText(period({ used: 0, total: 100 }))).toBe('0 / 100')
    expect(usageText(period({ used: null, total: 100, remaining: 79 }))).toBe('剩余 79 / 100')
  })
})

describe('百分比文案', () => {
  it('不把 0.4% 显示成 0%，不把 99.6% 显示成 100%', () => {
    expect(formatRemainingPct(0.4)).toBe('<1%')
    expect(formatRemainingPct(99.6)).toBe('99%')
    expect(formatRemainingPct(100)).toBe('100%')
    expect(formatRemainingPct(65.4)).toBe('65%')
  })
})

describe('倒计时', () => {
  const now = Date.parse('2026-10-14T00:00:00Z')
  const iso = (ms: number) => new Date(now + ms).toISOString()
  it('格式', () => {
    expect(formatCountdown(iso(5 * 3600_000), now)).toBe('5h 0m')
    expect(formatCountdown(iso(86400_000 + 12 * 3600_000 + 36 * 60_000), now)).toBe('1d 12h 36m')
    expect(formatCountdown(iso(7 * 86400_000), now)).toBe('7d 0h 0m')
    expect(formatCountdown(iso(20 * 60_000), now)).toBe('20m')
  })
  it('未知 -> null；已过 -> 待重置', () => {
    expect(formatCountdown(null, now)).toBeNull()
    expect(formatCountdown('garbage', now)).toBeNull()
    expect(formatCountdown(iso(-1000), now)).toBe('待重置')
  })
})

describe('账号分类', () => {
  it('空账号：所有已知额度都为 0', () => {
    expect(isEmptyAccount(acct({ periods: [period({ used: 100, remaining: 0 })] }))).toBe(true)
    expect(isEmptyAccount(acct({ balances: [{ currency: 'USD', amount: 0 }] }))).toBe(true)
  })
  it('含任一正额度 / 未知项 / 无数据 -> 不是空账号', () => {
    expect(isEmptyAccount(acct({ periods: [period({ remaining: 0, used: 100 }), period({ remaining: 5 })] }))).toBe(false)
    expect(isEmptyAccount(acct({ periods: [period({ used: null, total: null, remaining: null })] }))).toBe(false)
    expect(isEmptyAccount(acct({ balances: [{ currency: 'USD', amount: null }] }))).toBe(false)
    expect(isEmptyAccount(acct())).toBe(false)
  })
  it('可用账号：无错误且有正额度', () => {
    expect(isAvailableAccount(acct({ periods: [period({ remaining: 10 })] }))).toBe(true)
    expect(isAvailableAccount(acct({ balances: [{ currency: 'CNY', amount: 55.7 }] }))).toBe(true)
    expect(isAvailableAccount(acct({ periods: [period({ remaining: 10 })], error: 'boom' }))).toBe(false)
    expect(isAvailableAccount(acct({ periods: [period({ remaining: 0, used: 100 })] }))).toBe(false)
    expect(isAvailableAccount(acct({ periods: [period({ used: null, total: null, remaining: null })] }))).toBe(false)
  })
  it('soonestReset：忽略过去与未知，没有则 Infinity', () => {
    const now = Date.parse('2026-10-14T00:00:00Z')
    const a = acct({
      periods: [
        period({ resetsAt: new Date(now - 1000).toISOString() }),
        period({ resetsAt: new Date(now + 5000).toISOString() }),
        period({ resetsAt: new Date(now + 9000).toISOString() }),
        period({ resetsAt: null }),
      ],
    })
    expect(soonestReset(a, now)).toBe(now + 5000)
    expect(soonestReset(acct(), now)).toBe(Infinity)
  })
})
