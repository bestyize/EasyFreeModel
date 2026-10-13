import { defineStore } from 'pinia'
import { computed, reactive, ref } from 'vue'
import { api } from '@/api'
import type { Protocol, UsageFilter, UsageRange, UsageRequest, UsageSummary } from '@/types'

export type UsageTab = 'overview' | 'detail'
export type LoadStatus = 'idle' | 'loading' | 'ready' | 'error'

/** 概览页“最近请求”最多展示条数 */
export const RECENT_LIMIT = 10

export interface DetailFilter {
  providerId: string
  model: string
  status: '' | UsageRequest['status']
  protocol: '' | Protocol
  query: string
}

const emptyFilter = (): DetailFilter => ({ providerId: '', model: '', status: '', protocol: '', query: '' })

function errMsg(e: unknown): string {
  return e instanceof Error ? e.message : String(e)
}

/** 空字符串视为“不筛选”，避免把 '' 传给后端当作精确匹配 */
export function toUsageFilter(range: UsageRange, f: DetailFilter): UsageFilter {
  const out: UsageFilter = { range }
  if (f.providerId) out.providerId = f.providerId
  if (f.model.trim()) out.model = f.model.trim()
  if (f.status) out.status = f.status
  if (f.protocol) out.protocol = f.protocol
  if (f.query.trim()) out.query = f.query.trim()
  return out
}

function byTimeDesc(a: UsageRequest, b: UsageRequest): number {
  return Date.parse(b.timestamp) - Date.parse(a.timestamp)
}

export const useUsageStore = defineStore('usage', () => {
  const tab = ref<UsageTab>('overview')
  const range = ref<UsageRange>('today')

  const summary = ref<UsageSummary | null>(null)
  const summaryStatus = ref<LoadStatus>('idle')
  const summaryError = ref<string | null>(null)

  const recent = ref<UsageRequest[]>([])
  const recentStatus = ref<LoadStatus>('idle')
  const recentError = ref<string | null>(null)

  const filter = reactive<DetailFilter>(emptyFilter())
  const detailRows = ref<UsageRequest[]>([])
  const detailStatus = ref<LoadStatus>('idle')
  const detailError = ref<string | null>(null)

  const providerLabels = ref<Record<string, string>>({})

  // 防止慢请求的旧响应覆盖新结果
  let summarySeq = 0
  let recentSeq = 0
  let detailSeq = 0
  // 记录各数据集对应的查询键，切换 Tab 时只在过期时重新加载
  let summaryKey = ''
  let recentKey = ''
  let detailKey = ''

  const providerNames = computed<Record<string, string>>(() => {
    const names: Record<string, string> = { ...providerLabels.value }
    for (const p of summary.value?.providerShare ?? []) names[p.providerId] = p.name
    for (const p of summary.value?.requestDistribution ?? []) names[p.providerId] = p.name
    return names
  })

  const providerOptions = computed(() => {
    const ids = new Set<string>()
    for (const p of summary.value?.requestDistribution ?? []) ids.add(p.providerId)
    for (const r of detailRows.value) ids.add(r.providerId)
    if (filter.providerId) ids.add(filter.providerId)
    return [...ids].map((id) => ({ id, name: providerNames.value[id] ?? id }))
  })

  const modelOptions = computed(() => {
    const models = new Set<string>()
    for (const m of summary.value?.modelRanking ?? []) models.add(m.model)
    for (const r of detailRows.value) models.add(r.model)
    return [...models].sort()
  })

  async function loadProviderNames() {
    try {
      const list = await api.listProviders()
      providerLabels.value = Object.fromEntries(list.map((p) => [p.id, p.name]))
    } catch {
      // 名称只是展示增强，失败时回退为 id，不打断页面
    }
  }

  async function loadSummary() {
    const seq = ++summarySeq
    const key = range.value
    summaryStatus.value = 'loading'
    try {
      const data = await api.usageSummary(key)
      if (seq !== summarySeq) return
      summary.value = data
      summaryStatus.value = 'ready'
      summaryError.value = null
      summaryKey = key
    } catch (e) {
      if (seq !== summarySeq) return
      summaryStatus.value = 'error'
      summaryError.value = errMsg(e)
    }
  }

  async function loadRecent() {
    const seq = ++recentSeq
    const key = range.value
    recentStatus.value = 'loading'
    try {
      const rows = await api.listUsageRequests({ range: key })
      if (seq !== recentSeq) return
      recent.value = [...rows].sort(byTimeDesc).slice(0, RECENT_LIMIT)
      recentStatus.value = 'ready'
      recentError.value = null
      recentKey = key
    } catch (e) {
      if (seq !== recentSeq) return
      recentStatus.value = 'error'
      recentError.value = errMsg(e)
    }
  }

  async function loadDetail() {
    const seq = ++detailSeq
    const f = toUsageFilter(range.value, filter)
    const key = JSON.stringify(f)
    detailStatus.value = 'loading'
    try {
      const rows = await api.listUsageRequests(f)
      if (seq !== detailSeq) return
      detailRows.value = [...rows].sort(byTimeDesc)
      detailStatus.value = 'ready'
      detailError.value = null
      detailKey = key
    } catch (e) {
      if (seq !== detailSeq) return
      detailStatus.value = 'error'
      detailError.value = errMsg(e)
    }
  }

  /** 加载当前 Tab 所需数据；force 时忽略缓存键（重试/手动刷新） */
  async function load(force = false) {
    if (Object.keys(providerLabels.value).length === 0) void loadProviderNames()
    if (tab.value === 'overview') {
      const jobs: Promise<void>[] = []
      if (force || summaryKey !== range.value) jobs.push(loadSummary())
      if (force || recentKey !== range.value) jobs.push(loadRecent())
      await Promise.all(jobs)
    } else {
      const key = JSON.stringify(toUsageFilter(range.value, filter))
      // 详情页的模型选项依赖概览的排行，缺失时顺带取一次（失败不阻塞）
      if ((!summary.value || summaryKey !== range.value) && summaryStatus.value !== 'loading') void loadSummary()
      if (force || detailKey !== key) await loadDetail()
    }
  }

  function setTab(next: UsageTab) {
    tab.value = next
    return load()
  }

  function setRange(next: UsageRange) {
    range.value = next
    return load()
  }

  function setFilter(patch: Partial<DetailFilter>) {
    Object.assign(filter, patch)
    return load()
  }

  function resetFilter() {
    Object.assign(filter, emptyFilter())
    return load()
  }

  return {
    tab,
    range,
    summary,
    summaryStatus,
    summaryError,
    recent,
    recentStatus,
    recentError,
    filter,
    detailRows,
    detailStatus,
    detailError,
    providerNames,
    providerOptions,
    modelOptions,
    load,
    loadSummary,
    loadRecent,
    loadDetail,
    setTab,
    setRange,
    setFilter,
    resetFilter,
  }
})
