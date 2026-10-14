import { defineStore } from 'pinia'
import { computed, ref } from 'vue'
import { api } from '@/api'
import type {
  AddConnectionInput,
  Connection,
  CreateCustomProviderInput,
  ModelEntry,
  ProviderDetail,
  ProviderGroup,
  ProviderSummary,
  TestResult,
} from '@/types'

export type BatchState = { phase: 'testing' } | { phase: 'done'; ok: number; total: number }

function errMsg(e: unknown): string {
  return e instanceof Error ? e.message : String(e)
}

/** 按名称/ID 做本页过滤（忽略大小写） */
export function filterProviders(list: ProviderSummary[], query: string): ProviderSummary[] {
  const q = query.trim().toLowerCase()
  if (!q) return list
  return list.filter((p) => p.name.toLowerCase().includes(q) || p.id.toLowerCase().includes(q))
}

export const useProvidersStore = defineStore('providers', () => {
  const providers = ref<ProviderSummary[]>([])
  const loading = ref(false)
  const loaded = ref(false)
  const error = ref<string | null>(null)
  /** 连接测试结果，key = connectionId */
  const testResults = ref<Record<string, TestResult>>({})
  const testingIds = ref<Set<string>>(new Set())
  /** 批量测试状态，key = providerId */
  const batch = ref<Record<string, BatchState>>({})
  const testingGroup = ref<ProviderGroup | null>(null)
  /** 最近一次批量测试没有可测对象时的说明，开始新一轮测试时清空 */
  const batchNote = ref<string | null>(null)

  const byGroup = computed(() => (group: ProviderGroup) => providers.value.filter((p) => p.group === group))

  async function fetchProviders() {
    loading.value = true
    error.value = null
    try {
      providers.value = await api.listProviders()
      loaded.value = true
    } catch (e) {
      error.value = errMsg(e)
    } finally {
      loading.value = false
    }
  }

  function getDetail(id: string): Promise<ProviderDetail> {
    return api.getProvider(id)
  }

  /** 保存后重新拉取列表，保证页面展示的是持久化后的状态 */
  async function saveProvider(detail: ProviderDetail): Promise<ProviderDetail> {
    const saved = await api.saveProvider(detail)
    await fetchProviders()
    return saved
  }

  async function testConnection(connectionId: string): Promise<TestResult> {
    testingIds.value = new Set(testingIds.value).add(connectionId)
    let result: TestResult
    try {
      result = await api.testConnection(connectionId)
    } catch (e) {
      result = { connectionId, providerId: null, ok: false, latencyMs: null, error: errMsg(e) }
    } finally {
      const next = new Set(testingIds.value)
      next.delete(connectionId)
      testingIds.value = next
    }
    testResults.value = { ...testResults.value, [connectionId]: result }
    return result
  }

  /**
   * 新建自定义提供商。协议与 Base URL 由 Rust 侧最终校验；创建后重新拉取列表。
   */
  async function createCustomProvider(input: CreateCustomProviderInput): Promise<ProviderDetail> {
    const created = await api.createCustomProvider(input)
    await fetchProviders()
    return created
  }

  async function deleteCustomProvider(id: string): Promise<void> {
    await api.deleteCustomProvider(id)
    await fetchProviders()
  }

  /**
   * 录入 API Key 创建连接。注意：`input.apiKey` 只是透传给后端，store 不保存它、
   * 不写日志，也不会出现在 store 的任何状态里；返回值（Connection）不含密钥。
   */
  async function addConnection(input: AddConnectionInput): Promise<Connection> {
    const conn = await api.addConnection(input)
    await fetchProviders()
    return conn
  }

  async function updateConnectionCredential(connectionId: string, apiKey: string): Promise<void> {
    await api.updateConnectionCredential(connectionId, apiKey)
  }

  const fetchingModels = ref<Set<string>>(new Set())

  /**
   * 拉取某提供商的可用模型（优先上游 /models，失败回退内置预设模型库）并合并入库。
   * 返回最新模型列表，交给弹窗草稿直接替换。
   */
  async function fetchModels(providerId: string): Promise<ModelEntry[]> {
    fetchingModels.value = new Set(fetchingModels.value).add(providerId)
    try {
      return await api.fetchProviderModels(providerId)
    } finally {
      const next = new Set(fetchingModels.value)
      next.delete(providerId)
      fetchingModels.value = next
    }
  }

  /** 并发度受限的批量执行 */
  async function pool<T>(items: T[], limit: number, fn: (item: T) => Promise<void>) {
    const queue = [...items]
    const workers = Array.from({ length: Math.max(1, Math.min(limit, queue.length)) }, async () => {
      while (queue.length) {
        const item = queue.shift() as T
        await fn(item)
      }
    })
    await Promise.all(workers)
  }

  /**
   * 批量测试某组：只把“适配器已接入且已有连接”的提供商交给后端 testProviders，
   * 适配器未接入的直接跳过，不会得到任何“通过”状态。按 providerId 把结果归组到卡片上。
   */
  async function testGroup(group: ProviderGroup, concurrency = 3, chunkSize = 10) {
    const targets = providers.value.filter((p) => p.group === group && p.adapterReady && p.connectionCount > 0)
    batchNote.value = null
    if (!targets.length) {
      batchNote.value = '没有已添加连接的提供商可测试'
      return
    }
    testingGroup.value = group
    for (const p of targets) batch.value = { ...batch.value, [p.id]: { phase: 'testing' } }
    const chunks: ProviderSummary[][] = []
    for (let i = 0; i < targets.length; i += chunkSize) chunks.push(targets.slice(i, i + chunkSize))
    try {
      await pool(chunks, concurrency, async (chunk) => {
        let results: TestResult[]
        try {
          results = await api.testProviders(chunk.map((p) => p.id))
        } catch (e) {
          const error = errMsg(e)
          for (const p of chunk) {
            batch.value = { ...batch.value, [p.id]: { phase: 'done', ok: 0, total: p.connectionCount } }
          }
          batchNote.value = `批量测试失败：${error}`
          return
        }
        const next = { ...testResults.value }
        for (const r of results) if (r.connectionId) next[r.connectionId] = r
        testResults.value = next
        for (const p of chunk) {
          const mine = results.filter((r) => r.providerId === p.id && r.connectionId)
          batch.value = {
            ...batch.value,
            [p.id]: { phase: 'done', ok: mine.filter((r) => r.ok).length, total: mine.length },
          }
        }
      })
    } finally {
      testingGroup.value = null
    }
  }

  return {
    providers,
    loading,
    loaded,
    error,
    testResults,
    testingIds,
    batch,
    testingGroup,
    batchNote,
    byGroup,
    fetchProviders,
    getDetail,
    saveProvider,
    createCustomProvider,
    deleteCustomProvider,
    addConnection,
    updateConnectionCredential,
    testConnection,
    testGroup,
    fetchModels,
    fetchingModels,
  }
})
