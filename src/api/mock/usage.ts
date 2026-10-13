import type { UsageApi } from '../types'
import type { Protocol, UsageFilter, UsageMetric, UsageRange, UsageRequest, UsageSummary } from '@/types'

/**
 * 浏览器预览用 mock，仅在非 Tauri 环境生效。
 * 用确定性伪随机数生成约 1500 条请求，summary 与明细均由同一份数据计算，保证两者一致。
 * 全部是演示数据，不代表真实用量。
 */

const HOUR = 3600_000
const DAY = 24 * HOUR
const NOW = Date.now()

interface MockProvider {
  id: string
  name: string
  protocol: Protocol
  models: { name: string; inPerM: number; outPerM: number; cachePerM: number; weight: number }[]
}

// id 与 providers mock 的 slug 保持一致
const PROVIDERS: MockProvider[] = [
  {
    id: 'claude-code',
    name: 'Claude Code',
    protocol: 'anthropic-messages',
    models: [
      { name: 'claude-sonnet-4.5', inPerM: 3, outPerM: 15, cachePerM: 0.3, weight: 3 },
      { name: 'claude-haiku-4.5', inPerM: 1, outPerM: 5, cachePerM: 0.1, weight: 2 },
    ],
  },
  {
    id: 'openai-codex',
    name: 'OpenAI Codex',
    protocol: 'openai-responses',
    models: [
      { name: 'gpt-5-codex', inPerM: 1.25, outPerM: 10, cachePerM: 0.125, weight: 3 },
      { name: 'gpt-5-mini', inPerM: 0.25, outPerM: 2, cachePerM: 0.025, weight: 2 },
    ],
  },
  {
    id: 'gemini',
    name: 'Gemini',
    protocol: 'gemini',
    models: [
      { name: 'gemini-2.5-pro', inPerM: 1.25, outPerM: 10, cachePerM: 0.31, weight: 2 },
      { name: 'gemini-2.5-flash', inPerM: 0.3, outPerM: 2.5, cachePerM: 0.075, weight: 3 },
    ],
  },
  {
    id: 'openrouter',
    name: 'OpenRouter',
    protocol: 'openai-chat',
    models: [
      { name: 'deepseek/deepseek-chat-v3.1', inPerM: 0.27, outPerM: 1.1, cachePerM: 0.07, weight: 4 },
      { name: 'qwen/qwen3-coder', inPerM: 0.3, outPerM: 1.2, cachePerM: 0.0, weight: 1 },
    ],
  },
]

function mulberry32(seed: number): () => number {
  let a = seed
  return () => {
    a = (a + 0x6d2b79f5) | 0
    let t = Math.imul(a ^ (a >>> 15), 1 | a)
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296
  }
}

interface Row extends UsageRequest {
  protocol: Protocol
}

function pick<T extends { weight: number }>(rand: () => number, items: T[]): T {
  const total = items.reduce((s, i) => s + i.weight, 0)
  let r = rand() * total
  for (const i of items) {
    r -= i.weight
    if (r <= 0) return i
  }
  return items[items.length - 1]!
}

function generate(): Row[] {
  const rand = mulberry32(20261013)
  const rows: Row[] = []
  const providers = PROVIDERS.map((p, i) => ({ ...p, weight: [4, 3, 2, 2][i]! }))
  for (let i = 0; i < 1500; i++) {
    // 偏向近期：age = u^1.8 * 45 天
    const age = Math.pow(rand(), 1.8) * 45 * DAY
    const provider = pick(rand, providers)
    const model = pick(rand, provider.models)
    const input = Math.round(150 + Math.pow(rand(), 2.2) * 14000)
    const output = Math.round(40 + Math.pow(rand(), 2.5) * 3500)
    const cached = rand() < 0.6 ? Math.round(rand() * input * 3) : 0
    const roll = rand()
    const status: UsageRequest['status'] = roll < 0.93 ? 'success' : roll < 0.98 ? 'error' : 'cancelled'
    const cost =
      status === 'error'
        ? 0
        : (input * model.inPerM + output * model.outPerM + cached * model.cachePerM) / 1_000_000
    rows.push({
      requestId: `req_${(1_000_000 + i * 7919).toString(36)}`,
      providerId: provider.id,
      accountId: `${provider.id}-acct-${1 + Math.floor(rand() * 2)}`,
      model: model.name,
      inputTokens: input,
      outputTokens: status === 'error' ? 0 : output,
      cachedTokens: cached,
      latencyMs: Math.round(250 + Math.pow(rand(), 2) * 6000),
      status,
      estimatedCost: Number(cost.toFixed(6)),
      tokenSource: rand() < 0.15 ? 'estimated' : 'upstream',
      timestamp: new Date(NOW - age).toISOString(),
      protocol: provider.protocol,
    })
  }
  return rows.sort((a, b) => Date.parse(b.timestamp) - Date.parse(a.timestamp))
}

const ROWS = generate()

function startOfToday(): number {
  const d = new Date(NOW)
  d.setHours(0, 0, 0, 0)
  return d.getTime()
}

/** 当前周期与上一周期的 [start, end) 区间；'all' 没有上一周期 */
function windowOf(range: UsageRange): { start: number; end: number; prev: [number, number] | null } {
  switch (range) {
    case 'today': {
      const s = startOfToday()
      return { start: s, end: NOW, prev: [s - DAY, s - DAY + (NOW - s)] }
    }
    case '24h':
      return { start: NOW - DAY, end: NOW, prev: [NOW - 2 * DAY, NOW - DAY] }
    case '7d':
      return { start: NOW - 7 * DAY, end: NOW, prev: [NOW - 14 * DAY, NOW - 7 * DAY] }
    case '30d':
      return { start: NOW - 30 * DAY, end: NOW, prev: [NOW - 60 * DAY, NOW - 30 * DAY] }
    default:
      return { start: 0, end: NOW, prev: null }
  }
}

function inWindow(r: Row, start: number, end: number): boolean {
  const t = Date.parse(r.timestamp)
  return t >= start && t <= end
}

function metric(cur: number, prev: number | null): UsageMetric {
  // 上一周期为 0 或不存在时无法计算涨跌，返回 null 而不是编造 0%
  if (prev === null || prev === 0) return { value: cur, deltaPct: null }
  return { value: cur, deltaPct: Number((((cur - prev) / prev) * 100).toFixed(1)) }
}

function totals(rows: Row[]) {
  return rows.reduce(
    (a, r) => ({
      requests: a.requests + 1,
      input: a.input + r.inputTokens,
      output: a.output + r.outputTokens,
      cached: a.cached + r.cachedTokens,
      cost: a.cost + r.estimatedCost,
    }),
    { requests: 0, input: 0, output: 0, cached: 0, cost: 0 },
  )
}

const pad = (n: number) => String(n).padStart(2, '0')

function buildTrend(range: UsageRange, rows: Row[], start: number): UsageSummary['trend'] {
  const hourly = range === 'today' || range === '24h'
  const step = hourly ? HOUR : DAY
  let first = start
  if (range === 'all') {
    const oldest = rows.length ? Math.min(...rows.map((r) => Date.parse(r.timestamp))) : NOW
    const d = new Date(oldest)
    d.setHours(0, 0, 0, 0)
    first = d.getTime()
  } else if (range === '7d' || range === '30d') {
    const d = new Date(start)
    d.setHours(0, 0, 0, 0)
    first = d.getTime()
  } else if (range === '24h') {
    const d = new Date(start)
    d.setMinutes(0, 0, 0)
    first = d.getTime()
  }
  const buckets: UsageSummary['trend'] = []
  for (let t = first; t <= NOW; t += step) {
    const d = new Date(t)
    buckets.push({
      label: hourly ? `${pad(d.getHours())}:00` : `${pad(d.getMonth() + 1)}-${pad(d.getDate())}`,
      input: 0,
      output: 0,
      cached: 0,
      requests: 0,
      cost: 0,
    })
  }
  for (const r of rows) {
    const idx = Math.floor((Date.parse(r.timestamp) - first) / step)
    const b = buckets[idx]
    if (!b) continue
    b.input += r.inputTokens
    b.output += r.outputTokens
    b.cached += r.cachedTokens
    b.requests += 1
    b.cost = Number((b.cost + r.estimatedCost).toFixed(6))
  }
  return buckets
}

function share<K extends string>(map: Map<K, number>, total: number) {
  return [...map.entries()]
    .sort((a, b) => b[1] - a[1])
    .map(([key, value]) => ({ key, value, pct: total === 0 ? 0 : Number(((value / total) * 100).toFixed(1)) }))
}

const providerName = (id: string) => PROVIDERS.find((p) => p.id === id)?.name ?? id
const delay = (ms = 180) => new Promise((r) => setTimeout(r, ms))

export const usageMock: UsageApi = {
  usageSummary: async (range) => {
    await delay()
    const w = windowOf(range)
    const cur = ROWS.filter((r) => inWindow(r, w.start, w.end))
    const prev = w.prev ? ROWS.filter((r) => inWindow(r, w.prev![0], w.prev![1])) : null
    const t = totals(cur)
    const p = prev ? totals(prev) : null

    // 占比只统计输入+输出 Token，缓存 Token 单独展示，不并入
    const tokensByProvider = new Map<string, number>()
    const tokensByModel = new Map<string, number>()
    const requestsByProvider = new Map<string, number>()
    const modelProvider = new Map<string, string>()
    for (const r of cur) {
      const tok = r.inputTokens + r.outputTokens
      tokensByProvider.set(r.providerId, (tokensByProvider.get(r.providerId) ?? 0) + tok)
      tokensByModel.set(r.model, (tokensByModel.get(r.model) ?? 0) + tok)
      requestsByProvider.set(r.providerId, (requestsByProvider.get(r.providerId) ?? 0) + 1)
      modelProvider.set(r.model, r.providerId)
    }
    const totalTok = t.input + t.output

    return {
      range,
      requests: metric(t.requests, p?.requests ?? null),
      inputTokens: metric(t.input, p?.input ?? null),
      outputTokens: metric(t.output, p?.output ?? null),
      cachedTokens: metric(t.cached, p?.cached ?? null),
      estimatedCost: metric(Number(t.cost.toFixed(4)), p ? Number(p.cost.toFixed(4)) : null),
      costSource: '按各模型公开价目表 × 上游返回的 Token 数估算；标记“估算”的请求 Token 为本地估算，非账单',
      trend: buildTrend(range, cur, w.start),
      providerShare: share(tokensByProvider, totalTok).map((s) => ({
        providerId: s.key,
        name: providerName(s.key),
        tokens: s.value,
        pct: s.pct,
      })),
      modelRanking: share(tokensByModel, totalTok)
        .slice(0, 5)
        .map((s) => ({ model: s.key, providerId: modelProvider.get(s.key) ?? '', tokens: s.value, pct: s.pct })),
      requestDistribution: share(requestsByProvider, t.requests).map((s) => ({
        providerId: s.key,
        name: providerName(s.key),
        requests: s.value,
        pct: s.pct,
      })),
    }
  },

  listUsageRequests: async (filter: UsageFilter) => {
    await delay()
    const w = windowOf(filter.range)
    const q = filter.query?.trim().toLowerCase()
    const model = filter.model?.trim().toLowerCase()
    return ROWS.filter((r) => {
      if (!inWindow(r, w.start, w.end)) return false
      if (filter.providerId && r.providerId !== filter.providerId) return false
      if (model && !r.model.toLowerCase().includes(model)) return false
      if (filter.status && r.status !== filter.status) return false
      if (filter.protocol && r.protocol !== filter.protocol) return false
      if (q && !`${r.requestId} ${r.accountId} ${r.model} ${providerName(r.providerId)}`.toLowerCase().includes(q)) {
        return false
      }
      return true
    }).map(({ protocol: _protocol, ...req }) => req)
  },
}
