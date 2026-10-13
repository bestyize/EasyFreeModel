import { beforeEach, describe, expect, it, vi } from 'vitest'
import { createPinia, setActivePinia } from 'pinia'
import type { ProviderDetail, ProviderSummary } from '@/types'

const apiMock = vi.hoisted(() => ({
  listProviders: vi.fn(),
  getProvider: vi.fn(),
  saveProvider: vi.fn(),
  testConnection: vi.fn(),
  testProviders: vi.fn(),
  createCustomProvider: vi.fn(),
  deleteCustomProvider: vi.fn(),
  addConnection: vi.fn(),
  updateConnectionCredential: vi.fn(),
}))
vi.mock('@/api', () => ({ api: apiMock }))

import { filterProviders, useProvidersStore } from './providers'

function sum(id: string, over: Partial<ProviderSummary> = {}): ProviderSummary {
  return {
    id,
    name: id.toUpperCase(),
    group: 'oauth',
    status: 'disconnected',
    connectionCount: 0,
    adapterReady: false,
    protocols: [],
    quotaHint: null,
    serviceKinds: ['llm'],
    website: null,
    color: null,
    textIcon: null,
    authModes: ['apiKey'],
    pendingReason: null,
    ...over,
  }
}

function det(id: string): ProviderDetail {
  return {
    ...sum(id, { adapterReady: true, connectionCount: 2, status: 'connected' }),
    riskNotice: null,
    connections: [
      { id: `${id}-1`, providerId: id, accountLabel: 'a', kind: 'oauth', status: 'active', priority: 1, weight: 1, enabled: true },
      { id: `${id}-2`, providerId: id, accountLabel: 'b', kind: 'oauth', status: 'active', priority: 2, weight: 1, enabled: false },
    ],
    distribution: { roundRobin: true, maxConcurrency: 1, maxRetries: 0 },
    customHeaders: [],
    models: [],
    thinkingLevel: 'auto',
  }
}

beforeEach(() => {
  setActivePinia(createPinia())
  Object.values(apiMock).forEach((f) => f.mockReset())
})

describe('providers store', () => {
  it('loads providers and exposes error + retry', async () => {
    apiMock.listProviders.mockRejectedValueOnce(new Error('boom'))
    const s = useProvidersStore()
    await s.fetchProviders()
    expect(s.error).toBe('boom')
    expect(s.loaded).toBe(false)

    apiMock.listProviders.mockResolvedValueOnce([sum('a')])
    await s.fetchProviders()
    expect(s.error).toBeNull()
    expect(s.loaded).toBe(true)
    expect(s.byGroup('oauth')).toHaveLength(1)
  })

  it('re-fetches the list after saving', async () => {
    const d = det('a')
    apiMock.saveProvider.mockResolvedValue(d)
    apiMock.listProviders.mockResolvedValue([sum('a', { status: 'connected', adapterReady: true })])
    const s = useProvidersStore()
    const saved = await s.saveProvider(d)
    expect(saved).toEqual(d)
    expect(apiMock.saveProvider).toHaveBeenCalledWith(d)
    expect(apiMock.listProviders).toHaveBeenCalledTimes(1)
    expect(s.providers[0].status).toBe('connected')
  })

  it('does not swallow save errors', async () => {
    apiMock.saveProvider.mockRejectedValue(new Error('nope'))
    const s = useProvidersStore()
    await expect(s.saveProvider(det('a'))).rejects.toThrow('nope')
    expect(apiMock.listProviders).not.toHaveBeenCalled()
  })

  it('testGroup sends only ready providers that have connections to testProviders, grouped by providerId', async () => {
    const s = useProvidersStore()
    s.providers = [
      sum('ready', { adapterReady: true, connectionCount: 2 }),
      sum('todo', { connectionCount: 5 }),
      sum('empty', { adapterReady: true, connectionCount: 0 }),
    ]
    apiMock.testProviders.mockImplementation(async (ids: string[]) =>
      ids.flatMap((id) => [
        { connectionId: `${id}-1`, providerId: id, ok: false, latencyMs: null, error: '探测尚未接入' },
        { connectionId: `${id}-2`, providerId: id, ok: true, latencyMs: 10, error: null },
      ]),
    )
    await s.testGroup('oauth')
    expect(apiMock.testProviders).toHaveBeenCalledTimes(1)
    expect(apiMock.testProviders).toHaveBeenCalledWith(['ready'])
    expect(apiMock.getProvider).not.toHaveBeenCalled()
    expect(apiMock.testConnection).not.toHaveBeenCalled()
    expect(s.batch.ready).toEqual({ phase: 'done', ok: 1, total: 2 })
    expect(s.batch.todo).toBeUndefined()
    expect(s.batch.empty).toBeUndefined()
    expect(s.testResults['ready-1'].error).toBe('探测尚未接入')
    expect(s.testingGroup).toBeNull()
  })

  it('testGroup ignores provider-level results that have no connection id when counting', async () => {
    const s = useProvidersStore()
    s.providers = [sum('a', { adapterReady: true, connectionCount: 1 })]
    apiMock.testProviders.mockResolvedValue([
      { connectionId: '', providerId: 'a', ok: false, latencyMs: null, error: '没有已启用的连接' },
    ])
    await s.testGroup('oauth')
    expect(s.batch.a).toEqual({ phase: 'done', ok: 0, total: 0 })
    expect(Object.keys(s.testResults)).toEqual([])
  })

  it('testGroup explains itself when nothing is testable, and survives API failures', async () => {
    const s = useProvidersStore()
    s.providers = [sum('todo', { connectionCount: 5 })]
    await s.testGroup('oauth')
    expect(apiMock.testProviders).not.toHaveBeenCalled()
    expect(s.batchNote).toContain('没有已添加连接')

    s.providers = [sum('a', { adapterReady: true, connectionCount: 3 })]
    apiMock.testProviders.mockRejectedValue(new Error('offline'))
    await s.testGroup('oauth')
    expect(s.batch.a).toEqual({ phase: 'done', ok: 0, total: 3 })
    expect(s.batchNote).toContain('offline')
    expect(s.testingGroup).toBeNull()
  })

  it('addConnection passes the key straight through and never keeps it in store state', async () => {
    const SECRET = 'sk-test-SUPERSECRET-123'
    const conn = { id: 'c1', providerId: 'p', accountLabel: 'me', kind: 'apiKey', status: 'active', priority: 1, weight: 1, enabled: true }
    apiMock.addConnection.mockResolvedValue(conn)
    apiMock.listProviders.mockResolvedValue([sum('p', { adapterReady: true, connectionCount: 1 })])
    const s = useProvidersStore()
    const out = await s.addConnection({ providerId: 'p', accountLabel: 'me', apiKey: SECRET })
    expect(out).toEqual(conn)
    expect(apiMock.addConnection).toHaveBeenCalledWith({ providerId: 'p', accountLabel: 'me', apiKey: SECRET })
    expect(apiMock.listProviders).toHaveBeenCalledTimes(1)
    expect(JSON.stringify(s.$state)).not.toContain('SUPERSECRET')

    apiMock.updateConnectionCredential.mockResolvedValue(undefined)
    await s.updateConnectionCredential('c1', SECRET)
    expect(JSON.stringify(s.$state)).not.toContain('SUPERSECRET')
  })

  it('addConnection errors propagate and do not refresh or leak the key', async () => {
    apiMock.addConnection.mockRejectedValue(new Error('暂不能添加连接'))
    const s = useProvidersStore()
    await expect(s.addConnection({ providerId: 'p', accountLabel: 'x', apiKey: 'sk-LEAK' })).rejects.toThrow('暂不能添加连接')
    expect(apiMock.listProviders).not.toHaveBeenCalled()
    expect(JSON.stringify(s.$state)).not.toContain('sk-LEAK')
  })

  it('createCustomProvider / deleteCustomProvider re-fetch the list', async () => {
    apiMock.createCustomProvider.mockResolvedValue(det('custom-1'))
    apiMock.deleteCustomProvider.mockResolvedValue(undefined)
    apiMock.listProviders.mockResolvedValue([])
    const s = useProvidersStore()
    const d = await s.createCustomProvider({ name: 'X', protocol: 'openai-chat', baseUrl: 'https://a.example.com/v1' })
    expect(d.id).toBe('custom-1')
    expect(apiMock.createCustomProvider).toHaveBeenCalledWith({ name: 'X', protocol: 'openai-chat', baseUrl: 'https://a.example.com/v1' })
    await s.deleteCustomProvider('custom-1')
    expect(apiMock.deleteCustomProvider).toHaveBeenCalledWith('custom-1')
    expect(apiMock.listProviders).toHaveBeenCalledTimes(2)
  })

  it('records a failed test result when the API throws', async () => {
    apiMock.testConnection.mockRejectedValue(new Error('timeout'))
    const s = useProvidersStore()
    const r = await s.testConnection('c1')
    expect(r.ok).toBe(false)
    expect(s.testResults.c1.error).toBe('timeout')
  })
})

describe('filterProviders', () => {
  it('matches name case-insensitively', () => {
    const list = [sum('claude'), sum('kimi')]
    expect(filterProviders(list, ' KIM ').map((p) => p.id)).toEqual(['kimi'])
    expect(filterProviders(list, '')).toHaveLength(2)
  })
})
