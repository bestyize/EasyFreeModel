import { defineStore } from 'pinia'
import { ref } from 'vue'
import { api } from '@/api'
import type { ApiKeyInfo, ApiKeySecretResult, CreateApiKeyInput } from '@/types'
import { errorMessage } from '@/components/media/format'

/**
 * 注意：完整密钥（secret）只存在于 create / rotate 的返回值里，由调用方在弹窗中展示一次，
 * 绝不写入本 store 的任何状态，也不 console.log。
 */
export const useApiKeysStore = defineStore('apiKeys', () => {
  const keys = ref<ApiKeyInfo[]>([])
  const loading = ref(false)
  const loaded = ref(false)
  const loadError = ref<string | null>(null)
  /** 正在执行操作的密钥 id，用于禁用重复点击 */
  const busyIds = ref<string[]>([])

  function upsert(info: ApiKeyInfo) {
    const i = keys.value.findIndex((k) => k.id === info.id)
    if (i >= 0) keys.value[i] = info
    else keys.value.unshift(info)
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

  async function load() {
    loading.value = true
    try {
      keys.value = await api.listApiKeys()
      loadError.value = null
      loaded.value = true
    } catch (e) {
      loadError.value = errorMessage(e)
    } finally {
      loading.value = false
    }
  }

  async function create(input: CreateApiKeyInput): Promise<ApiKeySecretResult> {
    const result = await api.createApiKey(input)
    upsert(result.key)
    return result
  }

  function rotate(id: string): Promise<ApiKeySecretResult> {
    return withBusy(id, async () => {
      const result = await api.rotateApiKey(id)
      upsert(result.key)
      return result
    })
  }

  function setDisabled(id: string, disabled: boolean): Promise<void> {
    return withBusy(id, async () => {
      upsert(await api.setApiKeyDisabled(id, disabled))
    })
  }

  function remove(id: string): Promise<void> {
    return withBusy(id, async () => {
      await api.deleteApiKey(id)
      keys.value = keys.value.filter((k) => k.id !== id)
    })
  }

  return { keys, loading, loaded, loadError, busyIds, load, create, rotate, setDisabled, remove }
})
