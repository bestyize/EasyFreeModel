import { describe, expect, it } from 'vitest'
import { mount } from '@vue/test-utils'
import type { UsageRequest } from '@/types'
import RequestTable from './RequestTable.vue'

const mkRows = (n: number, over: Partial<UsageRequest> = {}): UsageRequest[] =>
  Array.from({ length: n }, (_, i) => ({
    requestId: `req_${i}`,
    providerId: 'openrouter',
    accountId: 'acct-1',
    model: 'gpt-5-mini',
    inputTokens: 1000,
    outputTokens: 200,
    cachedTokens: 3000,
    latencyMs: 400,
    status: 'success',
    estimatedCost: 0.01,
    tokenSource: 'upstream',
    timestamp: new Date(Date.UTC(2026, 9, 14, 0, 0, 0) - i * 1000).toISOString(),
    ...over,
  }))

describe('RequestTable', () => {
  it('tokenSource=estimated 的行标注“估算”，upstream 不标', () => {
    const w = mount(RequestTable, { props: { rows: [...mkRows(1, { tokenSource: 'estimated' }), ...mkRows(1, { requestId: 'x' })] } })
    const rows = w.findAll('[data-test="row"]')
    expect(rows[0]!.text()).toContain('估算')
    expect(rows[1]!.text()).not.toContain('估算')
  })

  it('紧凑模式同样标注估算，并分别展示输入/输出（不求和）', () => {
    const w = mount(RequestTable, { props: { compact: true, rows: mkRows(1, { tokenSource: 'estimated' }) } })
    const t = w.text()
    expect(t).toContain('估算')
    expect(t).toContain('↑1,000')
    expect(t).toContain('↓200')
    expect(t).not.toContain('1,200')
    expect(t).not.toContain('4,200')
  })

  it('详情模式缓存 Token 独立成列，不并入输入', () => {
    const w = mount(RequestTable, { props: { rows: mkRows(1) } })
    const cells = w.findAll('[data-test="row"] td').map((c) => c.text())
    expect(cells).toContain('1,000')
    expect(cells).toContain('3,000')
    expect(cells).not.toContain('4,000')
  })

  it('长列表分页：只渲染一页，可翻页', async () => {
    const w = mount(RequestTable, { props: { rows: mkRows(60), pageSize: 25 } })
    expect(w.findAll('[data-test="row"]')).toHaveLength(25)
    expect(w.get('[data-test="page-indicator"]').text()).toBe('1 / 3')
    await w.get('button[aria-label="下一页"]').trigger('click')
    expect(w.get('[data-test="page-indicator"]').text()).toBe('2 / 3')
    await w.get('button[aria-label="下一页"]').trigger('click')
    expect(w.findAll('[data-test="row"]')).toHaveLength(10)
    expect(w.get('button[aria-label="下一页"]').attributes('disabled')).toBeDefined()
  })

  it('数据集变化后回到第一页', async () => {
    const w = mount(RequestTable, { props: { rows: mkRows(60), pageSize: 25 } })
    await w.get('button[aria-label="下一页"]').trigger('click')
    await w.setProps({ rows: mkRows(30) })
    expect(w.get('[data-test="page-indicator"]').text()).toBe('1 / 2')
  })

  it('提供商名缺失时回退为 id', () => {
    const w = mount(RequestTable, { props: { rows: mkRows(1), providerNames: {} } })
    expect(w.text()).toContain('openrouter')
    const w2 = mount(RequestTable, { props: { rows: mkRows(1), providerNames: { openrouter: 'OpenRouter' } } })
    expect(w2.text()).toContain('OpenRouter')
  })
})
