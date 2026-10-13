import { beforeEach, describe, expect, it, vi } from 'vitest'
import { flushPromises, mount } from '@vue/test-utils'
import { createPinia, setActivePinia } from 'pinia'
import type { UsageRequest, UsageSummary } from '@/types'

const apiMock = vi.hoisted(() => ({
  usageSummary: vi.fn(),
  listUsageRequests: vi.fn(),
  listProviders: vi.fn(),
}))
vi.mock('@/api', () => ({ api: apiMock }))
// jsdom 没有 canvas，图表实例化在此跳过（选项构造由 chartOptions.test 覆盖）
vi.mock('./useECharts', () => ({ useECharts: () => ({ chart: { value: null } }) }))

import UsageView from '@/views/UsageView.vue'

const summary = (over: Partial<UsageSummary> = {}): UsageSummary => ({
  range: 'today',
  requests: { value: 210, deltaPct: 12 },
  inputTokens: { value: 48_926_179, deltaPct: null },
  outputTokens: { value: 141_179, deltaPct: null },
  cachedTokens: { value: 48_130_816, deltaPct: null },
  estimatedCost: { value: 0.29, deltaPct: null },
  costSource: '公开价目表估算',
  trend: [{ label: '00:00', input: 1, output: 1, cached: 1, requests: 1, cost: 0 }],
  providerShare: [{ providerId: 'p', name: 'Prov', tokens: 100, pct: 100 }],
  modelRanking: [{ model: 'm1', providerId: 'p', tokens: 100, pct: 100 }],
  requestDistribution: [{ providerId: 'p', name: 'Prov', requests: 5, pct: 100 }],
  ...over,
})

const req = (over: Partial<UsageRequest> = {}): UsageRequest => ({
  requestId: 'r1',
  providerId: 'p',
  accountId: 'a',
  model: 'm1',
  inputTokens: 10,
  outputTokens: 5,
  cachedTokens: 0,
  latencyMs: 100,
  status: 'success',
  estimatedCost: 0.001,
  tokenSource: 'estimated',
  timestamp: new Date().toISOString(),
  ...over,
})

beforeEach(() => {
  setActivePinia(createPinia())
  for (const f of Object.values(apiMock)) f.mockReset()
  apiMock.usageSummary.mockResolvedValue(summary())
  apiMock.listUsageRequests.mockResolvedValue([req()])
  apiMock.listProviders.mockResolvedValue([])
})

describe('UsageView', () => {
  it('渲染 5 张指标卡；成本卡显式标注估算来源；deltaPct=null 的卡不显示涨跌', async () => {
    const w = mount(UsageView)
    await flushPromises()
    const cards = w.findAll('.metric')
    expect(cards.map((c) => c.attributes('aria-label'))).toEqual(['请求总数', '输入 Token', '输出 Token', '缓存 Token', '估算成本'])
    expect(cards[4]!.get('[data-test="note"]').text()).toContain('估算 · 公开价目表估算')
    expect(cards[0]!.find('[data-test="delta"]').exists()).toBe(true)
    expect(cards[0]!.get('[data-test="delta"]').text()).toContain('+12%')
    for (const i of [1, 2, 3, 4]) expect(cards[i]!.find('[data-test="delta"]').exists()).toBe(false)
    expect(cards[3]!.text()).toContain('48,130,816')
    expect(cards[1]!.text()).toContain('48,926,179')
  })

  it('最近请求中估算的 Token 带“估算”标注', async () => {
    const w = mount(UsageView)
    await flushPromises()
    expect(w.get('.recent').text()).toContain('估算')
  })

  it('summary 加载失败：显示错误与重试', async () => {
    apiMock.usageSummary.mockRejectedValueOnce(new Error('db locked'))
    const w = mount(UsageView)
    await flushPromises()
    expect(w.text()).toContain('使用情况加载失败')
    expect(w.text()).toContain('db locked')
    await w.findAll('button').find((b) => b.text() === '重试')!.trigger('click')
    await flushPromises()
    expect(w.findAll('.metric')).toHaveLength(5)
  })

  it('无数据：显示空状态而不是一排 0', async () => {
    apiMock.usageSummary.mockResolvedValue(
      summary({ requests: { value: 0, deltaPct: null }, trend: [], providerShare: [], modelRanking: [], requestDistribution: [] }),
    )
    const w = mount(UsageView)
    await flushPromises()
    expect(w.text()).toContain('该时间范围暂无使用数据')
    expect(w.findAll('.metric')).toHaveLength(0)
  })

  it('切换时间维度会按新范围请求', async () => {
    const w = mount(UsageView)
    await flushPromises()
    await w.findAll('[aria-label="时间维度"] button').find((b) => b.text() === '7天')!.trigger('click')
    await flushPromises()
    expect(apiMock.usageSummary).toHaveBeenLastCalledWith('7d')
  })

  it('详情 Tab：加载明细并可导出 CSV（Blob）', async () => {
    const created: Blob[] = []
    const urlApi = URL as unknown as Record<string, unknown>
    urlApi.createObjectURL = (b: Blob) => (created.push(b), 'blob:mock')
    urlApi.revokeObjectURL = () => {}
    // jsdom 不实现点击下载链接导致的导航，避免噪音
    const click = vi.spyOn(HTMLAnchorElement.prototype, 'click').mockImplementation(() => {})
    const w = mount(UsageView, { attachTo: document.body })
    await flushPromises()
    await w.findAll('[aria-label="使用情况视图"] button').find((b) => b.text() === '详情')!.trigger('click')
    await flushPromises()
    expect(w.findAll('[data-test="row"]')).toHaveLength(1)
    await w.get('[data-test="export"]').trigger('click')
    expect(created).toHaveLength(1)
    expect(created[0]!.type).toContain('text/csv')
    const text = await created[0]!.text()
    expect(text).toContain('request_id,provider_id,account_id,model')
    expect(text).toContain('r1,p,a,m1,10,5,0,100,success,0.001')
    expect(click).toHaveBeenCalledTimes(1)
    click.mockRestore()
    delete urlApi.createObjectURL
    delete urlApi.revokeObjectURL
    w.unmount()
  })
})
