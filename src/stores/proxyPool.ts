import { defineStore } from 'pinia'
import { computed, ref } from 'vue'
import { api } from '@/api'
import type { ProxyImportResult, ProxyNode, ProxyNodeInput, ProxyPoolConfig, ProxyPoolState } from '@/types'
import { errorMessage } from '@/components/media/format'

function cloneConfig(c: ProxyPoolConfig): ProxyPoolConfig {
  return { ...c, providerIds: [...c.providerIds] }
}

/** 草稿与已保存配置是否一致（enabled 由顶部开关即时保存，不计入草稿差异） */
export function sameConfig(a: ProxyPoolConfig, b: ProxyPoolConfig): boolean {
  const { enabled: _a, providerIds: pa, ...ra } = a
  const { enabled: _b, providerIds: pb, ...rb } = b
  return (
    (Object.keys(ra) as (keyof typeof ra)[]).every((k) => ra[k] === rb[k]) &&
    pa.length === pb.length &&
    [...pa].sort().every((id, i) => id === [...pb].sort()[i])
  )
}

export const useProxyPoolStore = defineStore('proxyPool', () => {
  const saved = ref<ProxyPoolConfig | null>(null)
  const draft = ref<ProxyPoolConfig | null>(null)
  const nodes = ref<ProxyNode[]>([])
  const loading = ref(false)
  const loaded = ref(false)
  const loadError = ref<string | null>(null)
  const savingConfig = ref(false)
  const testingIds = ref<string[]>([])
  const testingAll = ref(false)
  /** 正在执行操作的节点 id */
  const busyIds = ref<string[]>([])

  const dirty = computed(() => !!saved.value && !!draft.value && !sameConfig(saved.value, draft.value))
  const usableCount = computed(() => nodes.value.filter((n) => n.enabled && n.status !== 'cooldown').length)

  function apply(state: ProxyPoolState, keepDraft = false) {
    const wasDirty = dirty.value
    saved.value = cloneConfig(state.config)
    if (!draft.value || !(keepDraft && wasDirty)) draft.value = cloneConfig(state.config)
    else draft.value.enabled = state.config.enabled
    nodes.value = state.nodes
  }

  async function load() {
    if (loading.value) return
    loading.value = true
    try {
      apply(await api.getProxyPool(), true)
      loadError.value = null
      loaded.value = true
    } catch (e) {
      loadError.value = errorMessage(e)
    } finally {
      loading.value = false
    }
  }

  async function saveConfig(): Promise<void> {
    if (!draft.value || savingConfig.value) return
    savingConfig.value = true
    try {
      apply(await api.saveProxyPoolConfig(cloneConfig(draft.value)))
    } finally {
      savingConfig.value = false
    }
  }

  /** 顶部总开关：只改 enabled，其余沿用已保存配置 */
  async function setEnabled(enabled: boolean): Promise<void> {
    if (!saved.value) return
    apply(await api.saveProxyPoolConfig({ ...cloneConfig(saved.value), enabled }), true)
  }

  function resetDraft() {
    if (saved.value) draft.value = cloneConfig(saved.value)
  }

  async function withBusy<T>(id: string, fn: () => Promise<T>): Promise<T> {
    if (busyIds.value.includes(id)) throw new Error('操作进行中，请稍候')
    busyIds.value.push(id)
    try {
      return await fn()
    } finally {
      busyIds.value = busyIds.value.filter((x) => x !== id)
    }
  }

  async function saveNode(input: ProxyNodeInput): Promise<void> {
    apply(await api.saveProxyNode(input), true)
  }

  function remove(id: string): Promise<void> {
    return withBusy(id, async () => apply(await api.deleteProxyNode(id), true))
  }

  function setNodeEnabled(id: string, enabled: boolean): Promise<void> {
    return withBusy(id, async () => apply(await api.setProxyNodeEnabled(id, enabled), true))
  }

  function move(id: string, delta: number): Promise<void> {
    return withBusy(id, async () => apply(await api.moveProxyNode(id, delta), true))
  }

  async function importNodes(text: string): Promise<ProxyImportResult> {
    const result = await api.importProxyNodes(text)
    await load()
    return result
  }

  async function test(ids: string[]): Promise<void> {
    testingIds.value = [...new Set([...testingIds.value, ...ids])]
    try {
      nodes.value = await api.testProxyNodes(ids)
    } finally {
      testingIds.value = testingIds.value.filter((x) => !ids.includes(x))
    }
  }

  async function testAll(): Promise<void> {
    if (testingAll.value) return
    testingAll.value = true
    try {
      nodes.value = await api.testProxyNodes()
    } finally {
      testingAll.value = false
    }
  }

  return {
    saved, draft, nodes, loading, loaded, loadError, savingConfig, testingIds, testingAll, busyIds,
    dirty, usableCount,
    load, saveConfig, setEnabled, resetDraft, saveNode, remove, setNodeEnabled, move, importNodes, test, testAll,
  }
})
