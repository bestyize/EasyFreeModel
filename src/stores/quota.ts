import { defineStore } from 'pinia'
import { computed, ref } from 'vue'
import { api } from '@/api'
import { isAvailableAccount, isEmptyAccount, soonestReset } from '@/components/quota/quota'
import { useUiStore } from '@/stores/ui'
import type { QuotaAccount } from '@/types'

/** 自动刷新间隔（秒） */
export const AUTO_REFRESH_SECONDS = 60

export type QuotaStatus = 'idle' | 'loading' | 'ready' | 'error'
export type QuotaSort = 'expiring' | 'default'

function errMsg(e: unknown): string {
  return e instanceof Error ? e.message : String(e)
}

/**
 * 合并刷新结果。刷新失败（next.error 非空）时保留最后一次成功的数据：
 * lastSyncedAt 与（在新结果没带周期/余额时）周期和余额都沿用旧值，只更新 error。
 */
export function mergeAccount(prev: QuotaAccount | undefined, next: QuotaAccount): QuotaAccount {
  if (!prev || !next.error) return next
  return {
    ...next,
    lastSyncedAt: next.lastSyncedAt ?? prev.lastSyncedAt,
    periods: next.periods.length > 0 ? next.periods : prev.periods,
    balances: next.balances.length > 0 ? next.balances : prev.balances,
  }
}

export const useQuotaStore = defineStore('quota', () => {
  const accounts = ref<QuotaAccount[]>([])
  const status = ref<QuotaStatus>('idle')
  const loadError = ref<string | null>(null)

  const refreshing = ref(false)
  const refreshError = ref<string | null>(null)
  const syncingIds = ref<string[]>([])
  const togglingIds = ref<string[]>([])
  const notice = ref<string | null>(null)

  const providerFilter = ref('')
  const accountFilter = ref('')
  const sort = ref<QuotaSort>('expiring')
  /** 排序基准时间：只在数据加载时更新，避免卡片每分钟自己乱跳 */
  const sortedAt = ref(Date.now())

  const autoRefresh = ref(true)
  const countdown = ref(AUTO_REFRESH_SECONDS)
  let active = false
  let timer: ReturnType<typeof setInterval> | undefined
  let loadSeq = 0

  // ---- 派生 ----
  const providerOptions = computed(() => {
    const seen = new Map<string, string>()
    for (const a of accounts.value) if (!seen.has(a.providerId)) seen.set(a.providerId, a.providerName)
    return [...seen].map(([id, name]) => ({ id, name }))
  })

  const accountOptions = computed(() =>
    accounts.value
      .filter((a) => !providerFilter.value || a.providerId === providerFilter.value)
      .map((a) => ({ id: a.connectionId, label: `${a.providerName} · ${a.accountLabel}` })),
  )

  const visible = computed(() => {
    const ui = useUiStore()
    const q = ui.globalQuery.trim().toLowerCase()
    const list = accounts.value.filter((a) => {
      if (providerFilter.value && a.providerId !== providerFilter.value) return false
      if (accountFilter.value && a.connectionId !== accountFilter.value) return false
      if (q && !`${a.providerName} ${a.accountLabel}`.toLowerCase().includes(q)) return false
      return true
    })
    if (sort.value === 'expiring') {
      const now = sortedAt.value
      // Array.prototype.sort 稳定：重置时间相同/未知的账户保持原顺序
      return [...list].sort((a, b) => {
        const ta = soonestReset(a, now)
        const tb = soonestReset(b, now)
        if (ta === tb) return 0
        return ta < tb ? -1 : 1
      })
    }
    return list
  })

  const filtersActive = computed(() => Boolean(providerFilter.value || accountFilter.value))

  // ---- 筛选 ----
  function setProviderFilter(id: string) {
    providerFilter.value = id
    // 换了提供商后，原来选的账号若不属于它则清除
    if (accountFilter.value && !accountOptions.value.some((o) => o.id === accountFilter.value)) {
      accountFilter.value = ''
    }
  }
  function resetFilters() {
    providerFilter.value = ''
    accountFilter.value = ''
  }

  // ---- 加载 / 刷新 ----
  async function load() {
    const seq = ++loadSeq
    status.value = 'loading'
    try {
      const list = await api.listQuotaAccounts()
      if (seq !== loadSeq) return
      accounts.value = list
      sortedAt.value = Date.now()
      status.value = 'ready'
      loadError.value = null
    } catch (e) {
      if (seq !== loadSeq) return
      status.value = 'error'
      loadError.value = errMsg(e)
    }
  }

  function applyRefreshed(list: QuotaAccount[]) {
    const byId = new Map(list.map((a) => [a.connectionId, a]))
    const merged = accounts.value.map((a) => {
      const next = byId.get(a.connectionId)
      if (next) byId.delete(a.connectionId)
      return next ? mergeAccount(a, next) : a
    })
    accounts.value = [...merged, ...byId.values()]
    sortedAt.value = Date.now()
  }

  /** 刷新全部（手动按钮与自动刷新共用）。进行中的刷新不会重复触发。 */
  async function refreshAll() {
    if (refreshing.value) return
    refreshing.value = true
    countdown.value = AUTO_REFRESH_SECONDS
    try {
      applyRefreshed(await api.refreshQuota(null))
      refreshError.value = null
    } catch (e) {
      // 保留已有数据与 lastSyncedAt，只提示错误
      refreshError.value = errMsg(e)
    } finally {
      refreshing.value = false
    }
  }

  async function refreshOne(connectionId: string) {
    if (syncingIds.value.includes(connectionId)) return
    syncingIds.value = [...syncingIds.value, connectionId]
    try {
      applyRefreshed(await api.refreshQuota(connectionId))
    } catch (e) {
      const msg = errMsg(e)
      accounts.value = accounts.value.map((a) => (a.connectionId === connectionId ? { ...a, error: msg } : a))
    } finally {
      syncingIds.value = syncingIds.value.filter((id) => id !== connectionId)
    }
  }

  // ---- 启停 ----
  function patchEnabled(connectionId: string, enabled: boolean) {
    accounts.value = accounts.value.map((a) => (a.connectionId === connectionId ? { ...a, enabled } : a))
  }

  /** 乐观更新，失败回滚并返回是否成功 */
  async function setEnabled(connectionId: string, enabled: boolean): Promise<boolean> {
    if (togglingIds.value.includes(connectionId)) return false
    const before = accounts.value.find((a) => a.connectionId === connectionId)?.enabled
    togglingIds.value = [...togglingIds.value, connectionId]
    patchEnabled(connectionId, enabled)
    try {
      await api.setQuotaAccountEnabled(connectionId, enabled)
      return true
    } catch (e) {
      if (before !== undefined) patchEnabled(connectionId, before)
      notice.value = `${enabled ? '启用' : '停用'}失败：${errMsg(e)}`
      return false
    } finally {
      togglingIds.value = togglingIds.value.filter((id) => id !== connectionId)
    }
  }

  async function batchSet(targets: QuotaAccount[], enabled: boolean, noun: string) {
    const verb = enabled ? '开启' : '关闭'
    if (targets.length === 0) {
      notice.value = `没有需要${verb}的${noun}`
      return
    }
    const results = await Promise.all(targets.map((a) => setEnabled(a.connectionId, enabled)))
    const ok = results.filter(Boolean).length
    const failed = results.length - ok
    notice.value = failed === 0 ? `已${verb} ${ok} 个${noun}` : `已${verb} ${ok} 个${noun}，${failed} 个失败`
  }

  /** 关闭空账号：仅作用于当前仍启用的账号（遵循当前筛选范围） */
  function closeEmptyAccounts() {
    return batchSet(
      visible.value.filter((a) => a.enabled && isEmptyAccount(a)),
      false,
      '空账号',
    )
  }

  /** 开启可用账号：仅作用于当前已停用且可用的账号 */
  function enableAvailableAccounts() {
    return batchSet(
      visible.value.filter((a) => !a.enabled && isAvailableAccount(a)),
      true,
      '可用账号',
    )
  }

  function dismissNotice() {
    notice.value = null
  }

  // ---- 自动刷新定时器 ----
  function stopTimer() {
    if (timer !== undefined) clearInterval(timer)
    timer = undefined
  }

  function syncTimer() {
    if (active && autoRefresh.value) {
      if (timer !== undefined) return
      countdown.value = AUTO_REFRESH_SECONDS
      timer = setInterval(() => {
        countdown.value -= 1
        if (countdown.value <= 0) {
          countdown.value = AUTO_REFRESH_SECONDS
          void refreshAll()
        }
      }, 1000)
    } else {
      stopTimer()
    }
  }

  function setAutoRefresh(on: boolean) {
    autoRefresh.value = on
    syncTimer()
  }

  /** 页面可见时调用：按开关状态启动定时器 */
  function resume() {
    active = true
    syncTimer()
  }

  /** 页面隐藏/卸载时调用：无条件清理定时器，避免后台泄漏 */
  function suspend() {
    active = false
    stopTimer()
    notice.value = null
  }

  return {
    accounts,
    status,
    loadError,
    refreshing,
    refreshError,
    syncingIds,
    togglingIds,
    notice,
    providerFilter,
    accountFilter,
    sort,
    autoRefresh,
    countdown,
    providerOptions,
    accountOptions,
    visible,
    filtersActive,
    setProviderFilter,
    resetFilters,
    load,
    refreshAll,
    refreshOne,
    setEnabled,
    closeEmptyAccounts,
    enableAvailableAccounts,
    dismissNotice,
    setAutoRefresh,
    resume,
    suspend,
  }
})
