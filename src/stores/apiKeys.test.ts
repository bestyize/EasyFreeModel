import { createPinia, setActivePinia } from 'pinia'
import { beforeEach, describe, expect, it, vi } from 'vitest'

const h = vi.hoisted(() => ({
  api: { listApiKeys: vi.fn(), createApiKey: vi.fn(), rotateApiKey: vi.fn(), setApiKeyDisabled: vi.fn(), deleteApiKey: vi.fn() },
}))
vi.mock('@/api', () => ({ api: h.api }))

import { useApiKeysStore } from './apiKeys'

const info = (over = {}) => ({
  id: 'k', name: 'n', masked: 'sk-r…1234', protocols: ['openai-chat'], allowedModels: [], rpmLimit: null, tpmLimit: null,
  totalLimit: null, callCount: 0, createdAt: '2026-10-01T00:00:00Z', lastUsedAt: null, expiresAt: null, disabled: false, ...over,
})

beforeEach(() => {
  setActivePinia(createPinia())
  Object.values(h.api).forEach((f) => f.mockReset())
})

describe('apiKeys store', () => {
  it('create / rotate 的 secret 只通过返回值给调用方，不保存在 store 状态中', async () => {
    const s = useApiKeysStore()
    h.api.createApiKey.mockResolvedValue({ key: info(), secret: 'sk-r-TOPSECRET' })
    const created = await s.create({ name: 'n' } as never)
    expect(created.secret).toBe('sk-r-TOPSECRET')
    h.api.rotateApiKey.mockResolvedValue({ key: info({ masked: 'sk-r…9999' }), secret: 'sk-r-ROTATED' })
    await s.rotate('k')
    const dump = JSON.stringify(s.$state)
    expect(dump).not.toContain('TOPSECRET')
    expect(dump).not.toContain('ROTATED')
    expect(s.keys[0].masked).toBe('sk-r…9999')
  })

  it('同一密钥的操作进行中拒绝并发', async () => {
    const s = useApiKeysStore()
    s.keys = [info()] as never
    let resolve!: () => void
    h.api.deleteApiKey.mockReturnValue(new Promise<void>((r) => (resolve = r)))
    const p = s.remove('k')
    await expect(s.remove('k')).rejects.toThrow('操作进行中')
    resolve()
    await p
    expect(s.keys).toEqual([])
    expect(s.busyIds).toEqual([])
  })

  it('加载失败记录错误', async () => {
    h.api.listApiKeys.mockRejectedValue(new Error('db locked'))
    const s = useApiKeysStore()
    await s.load()
    expect(s.loadError).toBe('db locked')
  })
})
