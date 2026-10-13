import { beforeEach, describe, expect, it, vi } from 'vitest'
import { createPinia, setActivePinia } from 'pinia'
import type { UsageRequest, UsageSummary } from '@/types'

const apiMock = vi.hoisted(() => ({
  usageSummary: vi.fn(),
  listUsageRequests: vi.fn(),
  listProviders: vi.fn(),
}))
vi.mock('@/api', () => ({ api: apiMock }))

import { RECENT_LIMIT, toUsageFilter, useUsageStore } from './usage'

const summary = (over: Partial<UsageSummary> = {}): UsageSummary => ({
  range: 'today',
  requests: { value: 1, deltaPct: null },
  inputTokens: { value: 1, deltaPct: null },
  outputTokens: { value: 1, deltaPct: null },
  cachedTokens: { value: 1, deltaPct: null },
  estimatedCost: { value: 0, deltaPct: null },
  costSource: 'x',
  trend: [],
  providerShare: [],
  modelRanking: [],
  requestDistribution: [],
  ...over,
})

const req = (i: number, over: Partial<UsageRequest> = {}): UsageRequest => ({
  requestId: `r${i}`,
  providerId: 'p',
  accountId: 'a',
  model: 'm',
  inputTokens: 1,
  outputTokens: 1,
  cachedTokens: 0,
  latencyMs: 1,
  status: 'success',
  estimatedCost: 0,
  tokenSource: 'upstream',
  timestamp: new Date(Date.UTC(2026, 9, 14, 0, 0, i)).toISOString(),
  ...over,
})

beforeEach(() => {
  setActivePinia(createPinia())
  for (const f of Object.values(apiMock)) f.mockReset()
  apiMock.usageSummary.mockResolvedValue(summary())
  apiMock.listUsageRequests.mockResolvedValue([])
  apiMock.listProviders.mockResolvedValue([])
})

describe('toUsageFilter', () => {
  it('空值不传给后端', () => {
    expect(toUsageFilter('7d', { providerId: '', model: ' ', status: '', protocol: '', query: '' })).toEqual({ range: '7d' })
    expect(toUsageFilter('7d', { providerId: 'p', model: ' m ', status: 'error', protocol: 'gemini', query: 'q' })).toEqual({
      range: '7d',
      providerId: 'p',
      model: 'm',
      status: 'error',
      protocol: 'gemini',
      query: 'q',
    })
  })
})

describe('usage store', () => {
  it('概览加载 summary 与最近请求（按时间倒序，最多 RECENT_LIMIT 条）', async () => {
    apiMock.listUsageRequests.mockResolvedValue(Array.from({ length: 30 }, (_, i) => req(i)))
    const s = useUsageStore()
    await s.load()
    expect(s.summaryStatus).toBe('ready')
    expect(s.recent).toHaveLength(RECENT_LIMIT)
    expect(s.recent[0]!.requestId).toBe('r29')
  })

  it('加载失败进入 error 并可重试', async () => {
    apiMock.usageSummary.mockRejectedValueOnce(new Error('boom'))
    const s = useUsageStore()
    await s.load()
    expect(s.summaryStatus).toBe('error')
    expect(s.summaryError).toBe('boom')
    await s.load(true)
    expect(s.summaryStatus).toBe('ready')
    expect(s.summaryError).toBeNull()
  })

  it('切换时间范围重新加载', async () => {
    const s = useUsageStore()
    await s.load()
    await s.setRange('7d')
    expect(apiMock.usageSummary).toHaveBeenLastCalledWith('7d')
    expect(apiMock.listUsageRequests).toHaveBeenLastCalledWith({ range: '7d' })
  })

  it('范围未变时切回概览不重复请求', async () => {
    const s = useUsageStore()
    await s.load()
    const calls = apiMock.usageSummary.mock.calls.length
    await s.setTab('detail')
    await s.setTab('overview')
    expect(apiMock.usageSummary.mock.calls.length).toBe(calls)
  })

  it('详情页把筛选条件传给 API', async () => {
    const s = useUsageStore()
    await s.setTab('detail')
    await s.setFilter({ status: 'error', protocol: 'gemini' })
    expect(apiMock.listUsageRequests).toHaveBeenLastCalledWith({ range: 'today', status: 'error', protocol: 'gemini' })
    await s.resetFilter()
    expect(apiMock.listUsageRequests).toHaveBeenLastCalledWith({ range: 'today' })
  })

  it('慢请求的旧响应不会覆盖新结果', async () => {
    let resolveSlow!: (v: UsageRequest[]) => void
    apiMock.listUsageRequests
      .mockImplementationOnce(() => new Promise((r) => (resolveSlow = r)))
      .mockResolvedValueOnce([req(2)])
    const s = useUsageStore()
    s.tab = 'detail'
    const slow = s.loadDetail()
    await s.setFilter({ status: 'error' })
    resolveSlow([req(1)])
    await slow
    expect(s.detailRows.map((r) => r.requestId)).toEqual(['r2'])
  })

  it('提供商名称优先取 summary，其次 listProviders，最后回退 id', async () => {
    apiMock.listProviders.mockResolvedValue([{ id: 'p', name: 'From List' }, { id: 'q', name: 'Q' }])
    apiMock.usageSummary.mockResolvedValue(
      summary({ providerShare: [{ providerId: 'p', name: 'From Summary', tokens: 1, pct: 100 }] }),
    )
    const s = useUsageStore()
    await s.load()
    await vi.waitFor(() => expect(s.providerNames.q).toBe('Q'))
    expect(s.providerNames.p).toBe('From Summary')
    expect(s.providerNames.zzz).toBeUndefined()
  })
})
