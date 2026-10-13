import type { ApiKeysApi } from '../types'
import type { ApiKeyInfo, CreateApiKeyInput } from '@/types'

/**
 * 浏览器预览用的内存 mock（仅非 Tauri 环境生效）。
 * 完整密钥只在 create / rotate 的返回值里出现，内存里只保留掩码，与真实契约一致。
 */
const store: ApiKeyInfo[] = []

function randomSecret(): string {
  const bytes = new Uint8Array(24)
  crypto.getRandomValues(bytes)
  return 'sk-r-' + Array.from(bytes, (b) => b.toString(16).padStart(2, '0')).join('')
}

function mask(secret: string): string {
  return `${secret.slice(0, 4)}…${secret.slice(-4)}`
}

function find(id: string): ApiKeyInfo {
  const k = store.find((x) => x.id === id)
  if (!k) throw new Error('密钥不存在')
  return k
}

export const apiKeysMock: ApiKeysApi = {
  listApiKeys: async () => store.map((k) => ({ ...k })),
  createApiKey: async (input: CreateApiKeyInput) => {
    const secret = randomSecret()
    const key: ApiKeyInfo = {
      id: crypto.randomUUID(),
      name: input.name,
      masked: mask(secret),
      protocols: input.protocols,
      allowedModels: input.allowedModels,
      rpmLimit: input.rpmLimit,
      tpmLimit: input.tpmLimit,
      totalLimit: input.totalLimit,
      callCount: 0,
      createdAt: new Date().toISOString(),
      lastUsedAt: null,
      expiresAt: input.expiresAt,
      disabled: false,
    }
    store.unshift(key)
    return { key: { ...key }, secret }
  },
  rotateApiKey: async (id) => {
    const k = find(id)
    const secret = randomSecret()
    k.masked = mask(secret)
    return { key: { ...k }, secret }
  },
  setApiKeyDisabled: async (id, disabled) => {
    const k = find(id)
    k.disabled = disabled
    return { ...k }
  },
  deleteApiKey: async (id) => {
    find(id)
    store.splice(store.findIndex((x) => x.id === id), 1)
  },
}
