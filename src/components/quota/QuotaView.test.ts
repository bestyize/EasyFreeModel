import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { flushPromises, mount } from '@vue/test-utils'
import { createPinia, setActivePinia } from 'pinia'
import type { QuotaAccount } from '@/types'

const apiMock = vi.hoisted(() => ({
  listQuotaAccounts: vi.fn(),
  refreshQuota: vi.fn(),
  setQuotaAccountEnabled: vi.fn(),
}))
vi.mock('@/api', () => ({ api: apiMock }))

import QuotaView from '@/views/QuotaView.vue'

const acct = (id: string, over: Partial<QuotaAccount> = {}): QuotaAccount => ({
  connectionId: id,
  providerId: 'p',
  providerName: 'Provider',
  accountLabel: id,
  enabled: true,
  periods: [{ name: 'Weekly', unit: '', used: 1, total: 100, remaining: 99, resetsAt: null, source: 'live' }],
  balances: [],
  lastSyncedAt: null,
  error: null,
  ...over,
})

beforeEach(() => {
  setActivePinia(createPinia())
  for (const f of Object.values(apiMock)) f.mockReset()
  apiMock.refreshQuota.mockResolvedValue([])
  apiMock.setQuotaAccountEnabled.mockResolvedValue(undefined)
})
afterEach(() => {
  vi.useRealTimers()
})

describe('QuotaView', () => {
  it('渲染账户卡；卸载后清理全部定时器', async () => {
    vi.useFakeTimers()
    apiMock.listQuotaAccounts.mockResolvedValue([acct('a'), acct('b')])
    const w = mount(QuotaView)
    await flushPromises()
    expect(w.findAll('.quota-card')).toHaveLength(2)
    expect(vi.getTimerCount()).toBeGreaterThan(0) // 自动刷新 + 倒计时 now 定时器
    w.unmount()
    expect(vi.getTimerCount()).toBe(0)
  })

  it('加载失败显示错误与重试按钮，重试成功后显示卡片', async () => {
    apiMock.listQuotaAccounts.mockRejectedValueOnce(new Error('ipc down')).mockResolvedValueOnce([acct('a')])
    const w = mount(QuotaView)
    await flushPromises()
    expect(w.text()).toContain('配额加载失败')
    expect(w.text()).toContain('ipc down')
    await w.findAll('button').find((b) => b.text() === '重试')!.trigger('click')
    await flushPromises()
    expect(w.findAll('.quota-card')).toHaveLength(1)
    w.unmount()
  })

  it('没有账户时显示空状态', async () => {
    apiMock.listQuotaAccounts.mockResolvedValue([])
    const w = mount(QuotaView)
    await flushPromises()
    expect(w.text()).toContain('暂无配额账户')
    w.unmount()
  })

  it('关闭自动刷新开关后倒计时文案消失', async () => {
    apiMock.listQuotaAccounts.mockResolvedValue([acct('a')])
    const w = mount(QuotaView)
    await flushPromises()
    expect(w.get('.auto').text()).toMatch(/自动刷新 \(\d+s\)/)
    await w.get('.auto [role="switch"]').trigger('click')
    expect(w.get('.auto').text()).toBe('自动刷新')
    w.unmount()
  })

  it('账户很多时分批渲染，可“显示更多”', async () => {
    apiMock.listQuotaAccounts.mockResolvedValue(Array.from({ length: 45 }, (_, i) => acct(`a${i}`)))
    const w = mount(QuotaView)
    await flushPromises()
    expect(w.findAll('.quota-card')).toHaveLength(20)
    await w.get('button.more').trigger('click')
    expect(w.findAll('.quota-card')).toHaveLength(40)
    w.unmount()
  })
})
