import type { QuotaApi } from '../types'
import type { QuotaAccount, QuotaPeriod } from '@/types'

/**
 * 浏览器预览用 mock，仅在非 Tauri 环境生效。全部是演示数据，不代表真实账户额度。
 * 覆盖了：多周期、剩余 100%/65%/18%/5%/0%、未知值（null）、估算/缓存来源、
 * 仅货币余额、同步失败（保留上次成功时间）、已停用账户。
 */

const MIN = 60_000
const HOUR = 60 * MIN
const DAY = 24 * HOUR
const NOW = Date.now()

const at = (offsetMs: number) => new Date(NOW + offsetMs).toISOString()
const ago = (offsetMs: number) => new Date(NOW - offsetMs).toISOString()

function period(
  name: string,
  used: number | null,
  total: number | null,
  resetsInMs: number | null,
  opts: { unit?: string; source?: QuotaPeriod['source']; remaining?: number | null } = {},
): QuotaPeriod {
  const remaining =
    opts.remaining !== undefined ? opts.remaining : used !== null && total !== null ? total - used : null
  return {
    name,
    unit: opts.unit ?? 'requests',
    used,
    total,
    remaining,
    resetsAt: resetsInMs === null ? null : at(resetsInMs),
    source: opts.source ?? 'live',
  }
}

function account(
  connectionId: string,
  providerId: string,
  providerName: string,
  accountLabel: string,
  periods: QuotaPeriod[],
  extra: Partial<QuotaAccount> = {},
): QuotaAccount {
  return {
    connectionId,
    providerId,
    providerName,
    accountLabel,
    enabled: true,
    periods,
    balances: [],
    lastSyncedAt: ago(3 * MIN),
    error: null,
    ...extra,
  }
}

let accounts: QuotaAccount[] = [
  account('antigravity/a1', 'antigravity', 'Antigravity', 'demo.one@example.com', [
    period('Gemini (5h)', 0, 1000, 5 * HOUR),
    period('Claude & GPT (5h)', 0, 1000, 5 * HOUR),
    period('Gemini 3.1 Flash Image', 0, 1000, 5 * HOUR),
    period('Gemini (Weekly)', 346, 1000, DAY + 12 * HOUR + 36 * MIN),
    period('Claude & GPT (Weekly)', 0, 1000, 7 * DAY),
  ]),
  account('antigravity/a2', 'antigravity', 'Antigravity', 'demo.two@example.com', [
    period('Gemini (5h)', 0, 1000, 5 * HOUR),
    period('Claude & GPT (5h)', 820, 1000, 3 * HOUR + 10 * MIN),
    period('Gemini (Weekly)', 589, 1000, DAY + 3 * HOUR + 32 * MIN),
    period('Claude & GPT (Weekly)', 0, 1000, 7 * DAY),
  ]),
  account(
    'codebuddy-cn/c1',
    'codebuddy-cn',
    'CodeBuddy CN',
    'Account 1',
    [
      period('Monthly', 305.5, 500, 18 * DAY + HOUR + 22 * MIN, { unit: 'credits' }),
      period('Bonus Pack 1', 100, 100, 15 * DAY + 2 * HOUR, { unit: 'credits' }),
      period('Bonus Pack 2', 100, 100, 17 * DAY + 22 * HOUR, { unit: 'credits' }),
      period('Bonus Pack 3', 0, 100, 18 * DAY + 2 * HOUR, { unit: 'credits' }),
      period('Bonus Pack 4', 0, 100, 19 * DAY + 14 * HOUR, { unit: 'credits' }),
      period('Bonus Pack 5', 0, 100, 20 * DAY, { unit: 'credits' }),
      period('Bonus Pack 6', 12, 100, 21 * DAY, { unit: 'credits' }),
      period('Bonus Pack 7', 0, 100, 22 * DAY, { unit: 'credits' }),
    ],
    { lastSyncedAt: ago(40 * MIN) },
  ),
  account('codebuddy/c1', 'codebuddy', 'CodeBuddy', 'Account 1', [period('Monthly', 0, 100, 18 * DAY, { unit: 'credits' })]),
  account('openai-codex/o1', 'openai-codex', 'OpenAI Codex', 'demo.codex@example.com', [
    period('5h', 21, 100, HOUR + 16 * MIN, { unit: '%' }),
    period('Weekly', 18, 100, 6 * DAY + 15 * HOUR, { unit: '%' }),
  ]),
  account('deepseek/d1', 'deepseek', 'DeepSeek', 'demo.deepseek@example.com', [], {
    balances: [
      { currency: 'USD', amount: 0 },
      { currency: 'CNY', amount: 55.7 },
    ],
  }),
  account('github-copilot/g1', 'github-copilot', 'GitHub Copilot', 'demo-user', [
    period('Monthly', 120, 200, 12 * DAY + 6 * HOUR),
    period('Premium Requests', 1920, 2000, 12 * DAY + 6 * HOUR),
  ]),
  account('kiro-ai/k1', 'kiro-ai', 'Kiro AI', 'Account 1', [
    period('Daily Requests', 20, 100, 6 * HOUR + 32 * MIN),
    period('Monthly', 800, 1000, 12 * DAY + 6 * HOUR, { source: 'cached' }),
    period('Image Generation', 40, 100, 2 * DAY + 14 * HOUR, { source: 'estimated' }),
  ]),
  // 未知值：上游没返回用量。必须显示“—/未知”，不能显示成 0%
  account('qoder/q1', 'qoder', 'Qoder', 'unknown.quota@example.com', [
    period('Monthly', null, null, null, { remaining: null, source: 'unknown', unit: '' }),
    period('Daily', null, 50, 4 * HOUR, { remaining: null, source: 'unknown' }),
  ]),
  // 同步失败：保留上次成功的数据与时间，并显示错误
  account(
    'cline/cl1',
    'cline',
    'Cline',
    'stale.account@example.com',
    [period('Monthly', 4, 100, 9 * DAY, { unit: 'credits', source: 'cached' })],
    { lastSyncedAt: ago(5 * HOUR + 12 * MIN), error: '上游返回 401：登录已过期，请重新授权' },
  ),
  account('kimi/km1', 'kimi', 'Kimi', 'paused.account@example.com', [period('Weekly', 100, 100, 3 * DAY)], {
    enabled: false,
  }),
]

const clone = <T>(v: T): T => structuredClone(v)
const delay = (ms = 200) => new Promise((r) => setTimeout(r, ms))

export const quotaMock: QuotaApi = {
  listQuotaAccounts: async () => {
    await delay()
    return clone(accounts)
  },
  refreshQuota: async (connectionId) => {
    await delay(500)
    accounts = accounts.map((a) => {
      if (connectionId !== null && a.connectionId !== connectionId) return a
      if (a.error) return a // 演示：失败账户刷新后仍失败，lastSyncedAt 不变
      return { ...a, lastSyncedAt: new Date().toISOString() }
    })
    return clone(connectionId === null ? accounts : accounts.filter((a) => a.connectionId === connectionId))
  },
  setQuotaAccountEnabled: async (connectionId, enabled) => {
    await delay(120)
    accounts = accounts.map((a) => (a.connectionId === connectionId ? { ...a, enabled } : a))
  },
}
