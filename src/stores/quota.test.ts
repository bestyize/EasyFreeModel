import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { createPinia, setActivePinia } from 'pinia'
import type { QuotaAccount, QuotaPeriod } from '@/types'

const apiMock = vi.hoisted(() => ({
  listQuotaAccounts: vi.fn(),
  refreshQuota: vi.fn(),
  setQuotaAccountEnabled: vi.fn(),
}))
vi.mock('@/api', () => ({ api: apiMock }))

import { AUTO_REFRESH_SECONDS, mergeAccount, useQuotaStore } from './quota'
import { useUiStore } from './ui'

const period = (over: Partial<QuotaPeriod> = {}): QuotaPeriod => ({
  name: '5h',
  unit: '',
  used: 10,
  total: 100,
  remaining: 90,
  resetsAt: null,
  source: 'live',
  ...over,
})

const acct = (id: string, over: Partial<QuotaAccount> = {}): QuotaAccount => ({
  connectionId: id,
  providerId: 'p',
  providerName: 'P',
  accountLabel: id,
  enabled: true,
  periods: [period()],
  balances: [],
  lastSyncedAt: '2026-10-14T00:00:00.000Z',
  error: null,
  ...over,
})

beforeEach(() => {
  setActivePinia(createPinia())
  for (const f of Object.values(apiMock)) f.mockReset()
  apiMock.listQuotaAccounts.mockResolvedValue([])
  apiMock.refreshQuota.mockResolvedValue([])
  apiMock.setQuotaAccountEnabled.mockResolvedValue(undefined)
})
afterEach(() => {
  vi.useRealTimers()
})

describe('mergeAccount', () => {
  it('刷新失败时保留旧 lastSyncedAt 与旧周期，只更新 error', () => {
    const prev = acct('a', { lastSyncedAt: '2026-10-14T00:00:00.000Z' })
    const next = acct('a', { error: 'timeout', lastSyncedAt: null, periods: [] })
    const merged = mergeAccount(prev, next)
    expect(merged.error).toBe('timeout')
    expect(merged.lastSyncedAt).toBe('2026-10-14T00:00:00.000Z')
    expect(merged.periods).toEqual(prev.periods)
  })

  it('成功的结果直接采用新值（包括 error 清除）', () => {
    const prev = acct('a', { error: 'old' })
    const next = acct('a', { error: null, lastSyncedAt: '2026-10-15T00:00:00.000Z', periods: [period({ used: 50 })] })
    expect(mergeAccount(prev, next)).toEqual(next)
  })

  it('后端在失败时返回的新 lastSyncedAt 优先', () => {
    const merged = mergeAccount(acct('a'), acct('a', { error: 'x', lastSyncedAt: '2026-10-14T05:00:00.000Z' }))
    expect(merged.lastSyncedAt).toBe('2026-10-14T05:00:00.000Z')
  })
})

describe('quota store', () => {
  it('load 成功/失败状态', async () => {
    apiMock.listQuotaAccounts.mockResolvedValueOnce([acct('a')])
    const s = useQuotaStore()
    await s.load()
    expect(s.status).toBe('ready')
    expect(s.accounts).toHaveLength(1)

    apiMock.listQuotaAccounts.mockRejectedValueOnce(new Error('ipc down'))
    await s.load()
    expect(s.status).toBe('error')
    expect(s.loadError).toBe('ipc down')
    expect(s.accounts).toHaveLength(1) // 失败不清空已有数据
  })

  it('refreshAll 抛错时保留数据并记录错误', async () => {
    apiMock.listQuotaAccounts.mockResolvedValue([acct('a')])
    const s = useQuotaStore()
    await s.load()
    apiMock.refreshQuota.mockRejectedValueOnce(new Error('offline'))
    await s.refreshAll()
    expect(s.refreshError).toBe('offline')
    expect(s.accounts[0]!.lastSyncedAt).toBe('2026-10-14T00:00:00.000Z')
    expect(s.refreshing).toBe(false)
  })

  it('refreshOne 只更新目标账户；失败时给该账户写入 error 并保留 lastSyncedAt', async () => {
    apiMock.listQuotaAccounts.mockResolvedValue([acct('a'), acct('b')])
    const s = useQuotaStore()
    await s.load()
    apiMock.refreshQuota.mockRejectedValueOnce(new Error('401'))
    await s.refreshOne('a')
    expect(s.accounts[0]!.error).toBe('401')
    expect(s.accounts[0]!.lastSyncedAt).toBe('2026-10-14T00:00:00.000Z')
    expect(s.accounts[1]!.error).toBeNull()
    expect(s.syncingIds).toEqual([])
  })

  it('setEnabled 乐观更新，失败回滚', async () => {
    apiMock.listQuotaAccounts.mockResolvedValue([acct('a')])
    const s = useQuotaStore()
    await s.load()
    apiMock.setQuotaAccountEnabled.mockRejectedValueOnce(new Error('nope'))
    const ok = await s.setEnabled('a', false)
    expect(ok).toBe(false)
    expect(s.accounts[0]!.enabled).toBe(true)
    expect(s.notice).toContain('nope')
    expect(await s.setEnabled('a', false)).toBe(true)
    expect(s.accounts[0]!.enabled).toBe(false)
  })

  it('关闭空账号：只关闭“已知全为 0”的启用账号，未知的不动', async () => {
    const empty = acct('empty', { periods: [period({ used: 100, remaining: 0 })] })
    const unknown = acct('unknown', { periods: [period({ used: null, total: null, remaining: null })] })
    const healthy = acct('healthy')
    apiMock.listQuotaAccounts.mockResolvedValue([empty, unknown, healthy])
    const s = useQuotaStore()
    await s.load()
    await s.closeEmptyAccounts()
    expect(apiMock.setQuotaAccountEnabled).toHaveBeenCalledTimes(1)
    expect(apiMock.setQuotaAccountEnabled).toHaveBeenCalledWith('empty', false)
    expect(s.notice).toBe('已关闭 1 个空账号')
  })

  it('开启可用账号：只开启已停用、无错误且有正额度的账号', async () => {
    const a = acct('ok', { enabled: false })
    const b = acct('err', { enabled: false, error: 'x' })
    const c = acct('zero', { enabled: false, periods: [period({ used: 100, remaining: 0 })] })
    const d = acct('already', { enabled: true })
    apiMock.listQuotaAccounts.mockResolvedValue([a, b, c, d])
    const s = useQuotaStore()
    await s.load()
    await s.enableAvailableAccounts()
    expect(apiMock.setQuotaAccountEnabled.mock.calls).toEqual([['ok', true]])
  })

  it('没有目标时给出提示而不调用 API', async () => {
    apiMock.listQuotaAccounts.mockResolvedValue([acct('a')])
    const s = useQuotaStore()
    await s.load()
    await s.closeEmptyAccounts()
    expect(apiMock.setQuotaAccountEnabled).not.toHaveBeenCalled()
    expect(s.notice).toBe('没有需要关闭的空账号')
  })

  it('即将过期优先：按最近的未来重置时间排序，未知排最后，同值保持原序', async () => {
    const t = Date.now()
    const at = (ms: number) => period({ resetsAt: new Date(t + ms).toISOString() })
    apiMock.listQuotaAccounts.mockResolvedValue([
      acct('unknown', { periods: [period({ resetsAt: null })] }),
      acct('late', { periods: [at(9 * 3600_000)] }),
      acct('soon', { periods: [at(9 * 3600_000), at(3600_000)] }),
      acct('late2', { periods: [at(9 * 3600_000)] }),
    ])
    const s = useQuotaStore()
    await s.load()
    expect(s.visible.map((a) => a.connectionId)).toEqual(['soon', 'late', 'late2', 'unknown'])
    s.sort = 'default'
    expect(s.visible.map((a) => a.connectionId)).toEqual(['unknown', 'late', 'soon', 'late2'])
  })

  it('提供商/账号筛选；换提供商会清掉不属于它的账号筛选', async () => {
    apiMock.listQuotaAccounts.mockResolvedValue([
      acct('a1', { providerId: 'A', providerName: 'A' }),
      acct('b1', { providerId: 'B', providerName: 'B' }),
    ])
    const s = useQuotaStore()
    await s.load()
    s.setProviderFilter('A')
    expect(s.visible.map((a) => a.connectionId)).toEqual(['a1'])
    s.accountFilter = 'a1'
    s.setProviderFilter('B')
    expect(s.accountFilter).toBe('')
    expect(s.visible.map((a) => a.connectionId)).toEqual(['b1'])
  })

  it('全局搜索框过滤', async () => {
    apiMock.listQuotaAccounts.mockResolvedValue([
      acct('a1', { providerName: 'Antigravity' }),
      acct('b1', { providerName: 'Kimi' }),
    ])
    const s = useQuotaStore()
    await s.load()
    useUiStore().globalQuery = 'kim'
    expect(s.visible.map((a) => a.connectionId)).toEqual(['b1'])
  })
})

describe('自动刷新定时器', () => {
  it('每 AUTO_REFRESH_SECONDS 秒触发一次刷新，倒计时递减', async () => {
    vi.useFakeTimers()
    const s = useQuotaStore()
    s.resume()
    expect(s.countdown).toBe(AUTO_REFRESH_SECONDS)
    vi.advanceTimersByTime(1000)
    expect(s.countdown).toBe(AUTO_REFRESH_SECONDS - 1)
    expect(apiMock.refreshQuota).not.toHaveBeenCalled()
    await vi.advanceTimersByTimeAsync((AUTO_REFRESH_SECONDS - 1) * 1000)
    expect(apiMock.refreshQuota).toHaveBeenCalledTimes(1)
    expect(apiMock.refreshQuota).toHaveBeenCalledWith(null)
    s.suspend()
  })

  it('suspend（卸载）清理定时器，之后不再刷新', async () => {
    vi.useFakeTimers()
    const s = useQuotaStore()
    s.resume()
    expect(vi.getTimerCount()).toBe(1)
    s.suspend()
    expect(vi.getTimerCount()).toBe(0)
    await vi.advanceTimersByTimeAsync(AUTO_REFRESH_SECONDS * 3000)
    expect(apiMock.refreshQuota).not.toHaveBeenCalled()
  })

  it('关闭自动刷新开关停止定时器；重新打开恢复；重复 resume 不会叠加定时器', () => {
    vi.useFakeTimers()
    const s = useQuotaStore()
    s.resume()
    s.resume()
    expect(vi.getTimerCount()).toBe(1)
    s.setAutoRefresh(false)
    expect(vi.getTimerCount()).toBe(0)
    s.setAutoRefresh(true)
    expect(vi.getTimerCount()).toBe(1)
    s.suspend()
    expect(vi.getTimerCount()).toBe(0)
  })

  it('页面不可见时（未 resume）切换开关不会启动定时器', () => {
    vi.useFakeTimers()
    const s = useQuotaStore()
    s.setAutoRefresh(false)
    s.setAutoRefresh(true)
    expect(vi.getTimerCount()).toBe(0)
  })

  it('刷新进行中不会重复发起', async () => {
    let resolve!: (v: QuotaAccount[]) => void
    apiMock.refreshQuota.mockImplementation(() => new Promise((r) => (resolve = r)))
    const s = useQuotaStore()
    const first = s.refreshAll()
    void s.refreshAll()
    expect(apiMock.refreshQuota).toHaveBeenCalledTimes(1)
    resolve([])
    await first
    expect(s.refreshing).toBe(false)
  })
})
