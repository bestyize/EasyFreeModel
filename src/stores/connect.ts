import { defineStore } from 'pinia'
import { computed, ref, watch } from 'vue'
import { api } from '@/api'
import { errorMessage } from '@/components/media/format'
import { PROTOCOL_OPTIONS } from '@/components/settings/protocols'
import { useApiKeysStore } from '@/stores/apiKeys'
import type { ApiKeyInfo, GatewayModel } from '@/types'

const ALL_PROTOCOLS = PROTOCOL_OPTIONS.filter((p) => p.implemented).map((p) => p.id)

function isLive(k: ApiKeyInfo) {
  return !k.disabled && !(k.expiresAt && new Date(k.expiresAt).getTime() <= Date.now())
}

/** 接入用的密钥与模型选择。「API 密钥」页的接入面板改它，「API 接入」页的请求示例读它。 */
export const useConnectStore = defineStore('connect', () => {
  const keys = useApiKeysStore()
  const keyId = ref('')
  const model = ref('')
  const models = ref<GatewayModel[]>([])
  const modelsError = ref<string | null>(null)
  const creating = ref(false)
  const createError = ref<string | null>(null)

  /** 能直接拿来接入的密钥：未停用、未过期、本机保存了明文 */
  const usableKeys = computed(() => keys.keys.filter((k) => isLive(k) && k.secret))
  const legacyOnly = computed(() => usableKeys.value.length === 0 && keys.keys.some((k) => isLive(k) && !k.secret))
  const fullKey = computed(() => usableKeys.value.find((k) => k.id === keyId.value)?.secret ?? '')

  watch(
    usableKeys,
    (list) => {
      if (list.some((k) => k.id === keyId.value)) return
      const best = list.find((k) => ALL_PROTOCOLS.every((p) => k.protocols.includes(p))) ?? list[0]
      keyId.value = best?.id ?? ''
    },
    { immediate: true },
  )
  watch(models, (list) => {
    if (!model.value && list.length) model.value = list[0].id
  })

  async function loadModels() {
    try {
      models.value = await api.listGatewayModels()
      modelsError.value = null
    } catch (e) {
      modelsError.value = errorMessage(e)
    }
  }

  async function createKey() {
    creating.value = true
    createError.value = null
    try {
      await keys.create({
        name: 'FreeApiBox 接入',
        expiresAt: null,
        protocols: ALL_PROTOCOLS,
        allowedModels: [],
        rpmLimit: null,
        tpmLimit: null,
        totalLimit: null,
      })
    } catch (e) {
      createError.value = `创建密钥失败：${errorMessage(e)}`
    } finally {
      creating.value = false
    }
  }

  return { keyId, model, models, modelsError, creating, createError, usableKeys, legacyOnly, fullKey, loadModels, createKey }
})
