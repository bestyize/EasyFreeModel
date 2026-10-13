import { createPinia, setActivePinia } from 'pinia'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import type { OAuthProviderInfo } from '@/types'

const h = vi.hoisted(() => ({
  api: {
    oauthListProviders: vi.fn(),
    oauthSetClientCredentials: vi.fn(),
    oauthClearClientCredentials: vi.fn(),
    oauthStart: vi.fn(),
    oauthPoll: vi.fn(),
    oauthCancel: vi.fn(),
    oauthReauthorize: vi.fn(),
    oauthDisconnect: vi.fn(),
  },
}))
vi.mock('@/api', () => ({ api: h.api }))

import { OAuthApiError, oauthErrorText } from '@/api/oauthError'
import { useOAuthStore } from './oauth'

const info = (over: Partial<OAuthProviderInfo> = {}): OAuthProviderInfo => ({
  providerId: 'antigravity',
  flow: 'authorization-code',
  status: 'need_client_credentials',
  reason: null,
  requires: [],
  limitation: null,
  tokenExchangePending: false,
  riskNotice: null,
  clientIdRequired: true,
  clientSecret: 'required',
  supportsBaseUrl: false,
  clientConfigured: false,
  ...over,
})

const SECRET = 'GOCSPX-store-SUPERSECRET'

beforeEach(() => {
  setActivePinia(createPinia())
  Object.values(h.api).forEach((f) => f.mockReset())
})

describe('oauth store', () => {
  it('fetchProviders / infoFor', async () => {
    h.api.oauthListProviders.mockResolvedValue([info(), info({ providerId: 'claude', status: 'ready' })])
    const s = useOAuthStore()
    await s.fetchProviders()
    expect(s.loaded).toBe(true)
    expect(s.infoFor('claude')?.status).toBe('ready')
    expect(s.infoFor('nope')).toBeUndefined()
  })

  it('加载失败只记录映射后的中文文案', async () => {
    h.api.oauthListProviders.mockRejectedValue({ code: 'storage', message: `RAW ${SECRET}` })
    const s = useOAuthStore()
    await s.fetchProviders()
    expect(s.error).toBe(oauthErrorText('storage'))
    expect(JSON.stringify(s.$state)).not.toContain('RAW')
    expect(JSON.stringify(s.$state)).not.toContain(SECRET)
  })

  it('setClientCredentials：透传入参，不保存任何输入，用返回值替换列表项', async () => {
    h.api.oauthListProviders.mockResolvedValue([info()])
    h.api.oauthSetClientCredentials.mockResolvedValue(info({ status: 'ready', clientConfigured: true }))
    const s = useOAuthStore()
    await s.fetchProviders()
    await s.setClientCredentials({ providerId: 'antigravity', clientId: 'cid-123', clientSecret: SECRET })
    expect(h.api.oauthSetClientCredentials).toHaveBeenCalledTimes(1)
    expect(h.api.oauthSetClientCredentials).toHaveBeenCalledWith({ providerId: 'antigravity', clientId: 'cid-123', clientSecret: SECRET })
    expect(s.infoFor('antigravity')).toMatchObject({ status: 'ready', clientConfigured: true })
    const dump = JSON.stringify(s.$state)
    expect(dump).not.toContain(SECRET)
    expect(dump).not.toContain('cid-123')
  })

  it('setClientCredentials 失败时抛出 OAuthApiError，状态里也没有密钥', async () => {
    h.api.oauthSetClientCredentials.mockRejectedValue(new OAuthApiError('invalid_input'))
    const s = useOAuthStore()
    await expect(s.setClientCredentials({ providerId: 'antigravity', clientId: 'c', clientSecret: SECRET })).rejects.toMatchObject({ code: 'invalid_input' })
    expect(JSON.stringify(s.$state)).not.toContain(SECRET)
  })

  it('clearClientCredentials 替换列表项', async () => {
    h.api.oauthListProviders.mockResolvedValue([info({ status: 'ready', clientConfigured: true })])
    h.api.oauthClearClientCredentials.mockResolvedValue(info())
    const s = useOAuthStore()
    await s.fetchProviders()
    await s.clearClientCredentials('antigravity')
    expect(s.infoFor('antigravity')).toMatchObject({ status: 'need_client_credentials', clientConfigured: false })
  })

  it('start / reauthorize / poll / cancel / disconnect 直接转发给 API', async () => {
    const s = useOAuthStore()
    await s.start('claude', '工作')
    expect(h.api.oauthStart).toHaveBeenCalledWith('claude', '工作')
    await s.reauthorize('c1')
    expect(h.api.oauthReauthorize).toHaveBeenCalledWith('c1')
    await s.poll('s1')
    expect(h.api.oauthPoll).toHaveBeenCalledWith('s1')
    await s.cancel('s1')
    expect(h.api.oauthCancel).toHaveBeenCalledWith('s1')
    await s.disconnect('c1')
    expect(h.api.oauthDisconnect).toHaveBeenCalledWith('c1')
  })
})
