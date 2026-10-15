import { createPinia, setActivePinia } from 'pinia'
import { beforeEach, describe, expect, it, vi } from 'vitest'
import type { ProxyNode, ProxyPoolConfig, ProxyPoolState } from '@/types'

const h = vi.hoisted(() => ({
  api: {
    getProxyPool: vi.fn(), saveProxyPoolConfig: vi.fn(), saveProxyNode: vi.fn(), deleteProxyNode: vi.fn(),
    setProxyNodeEnabled: vi.fn(), moveProxyNode: vi.fn(), importProxyNodes: vi.fn(), testProxyNodes: vi.fn(),
  },
}))
vi.mock('@/api', () => ({ api: h.api }))

import { sameConfig, useProxyPoolStore } from './proxyPool'

const config: ProxyPoolConfig = {
  enabled: false, strategy: 'roundRobin', providerScope: 'all', providerIds: [], failureThreshold: 3, cooldownSeconds: 60,
  maxAttempts: 2, fallbackDirect: false, testUrl: 'https://example.com/trace',
}
const node = (over: Partial<ProxyNode> = {}): ProxyNode => ({
  id: 'n1', name: 'n1', scheme: 'http', host: 'h', port: 1, username: null, hasPassword: false, enabled: true, status: 'untested',
  latencyMs: null, lastCheckedAt: null, lastError: null, exitIp: null, successCount: 0, failureCount: 0, cooldownRemainingSeconds: 0, ...over,
})
const state = (c: Partial<ProxyPoolConfig> = {}, nodes: ProxyNode[] = [node()]): ProxyPoolState => ({ config: { ...config, ...c }, nodes })

beforeEach(() => {
  setActivePinia(createPinia())
  Object.values(h.api).forEach((f) => f.mockReset())
  h.api.getProxyPool.mockResolvedValue(state())
})

describe('sameConfig', () => {
  it('忽略 enabled 与 providerIds 顺序', () => {
    expect(sameConfig(config, { ...config, enabled: true })).toBe(true)
    expect(sameConfig({ ...config, providerIds: ['a', 'b'] }, { ...config, providerIds: ['b', 'a'] })).toBe(true)
    expect(sameConfig(config, { ...config, cooldownSeconds: 5 })).toBe(false)
    expect(sameConfig(config, { ...config, providerIds: ['a'] })).toBe(false)
  })
})

describe('proxyPool store', () => {
  it('加载后草稿与已保存分离，dirty / resetDraft 正常', async () => {
    const s = useProxyPoolStore()
    await s.load()
    expect(s.loaded).toBe(true)
    expect(s.dirty).toBe(false)
    s.draft!.strategy = 'failover'
    expect(s.dirty).toBe(true)
    s.resetDraft()
    expect(s.dirty).toBe(false)
  })

  it('有未保存草稿时重新加载不覆盖草稿，但同步 enabled', async () => {
    const s = useProxyPoolStore()
    await s.load()
    s.draft!.cooldownSeconds = 300
    h.api.getProxyPool.mockResolvedValue(state({ enabled: true }))
    await s.load()
    expect(s.draft!.cooldownSeconds).toBe(300)
    expect(s.draft!.enabled).toBe(true)
  })

  it('总开关只提交 enabled，沿用已保存的其它配置（不带上未保存草稿）', async () => {
    const s = useProxyPoolStore()
    await s.load()
    s.draft!.cooldownSeconds = 300
    h.api.saveProxyPoolConfig.mockResolvedValue(state({ enabled: true }))
    await s.setEnabled(true)
    expect(h.api.saveProxyPoolConfig).toHaveBeenCalledWith({ ...config, enabled: true })
    expect(s.saved!.enabled).toBe(true)
    expect(s.draft!.cooldownSeconds).toBe(300)
  })

  it('保存配置采用后端返回值并清除 dirty；失败保留草稿', async () => {
    const s = useProxyPoolStore()
    await s.load()
    s.draft!.maxAttempts = 4
    h.api.saveProxyPoolConfig.mockRejectedValueOnce(new Error('冷却时间需在 5–3600 秒之间'))
    await expect(s.saveConfig()).rejects.toThrow('冷却时间')
    expect(s.dirty).toBe(true)
    h.api.saveProxyPoolConfig.mockResolvedValueOnce(state({ maxAttempts: 4 }))
    await s.saveConfig()
    expect(s.dirty).toBe(false)
    expect(s.saved!.maxAttempts).toBe(4)
  })

  it('可用节点数排除停用与冷却中的节点', async () => {
    h.api.getProxyPool.mockResolvedValue(
      state({}, [node({ id: 'a' }), node({ id: 'b', enabled: false }), node({ id: 'c', status: 'cooldown' }), node({ id: 'd', status: 'unhealthy' })]),
    )
    const s = useProxyPoolStore()
    await s.load()
    expect(s.usableCount).toBe(2)
  })

  it('节点操作用后端返回的完整状态覆盖本地；测速更新节点并清理 testing 标记', async () => {
    const s = useProxyPoolStore()
    await s.load()
    h.api.setProxyNodeEnabled.mockResolvedValue(state({}, [node({ enabled: false })]))
    await s.setNodeEnabled('n1', false)
    expect(s.nodes[0].enabled).toBe(false)

    h.api.testProxyNodes.mockResolvedValue([node({ status: 'healthy', latencyMs: 80 })])
    await s.test(['n1'])
    expect(s.nodes[0].latencyMs).toBe(80)
    expect(s.testingIds).toEqual([])
    await s.testAll()
    expect(h.api.testProxyNodes).toHaveBeenLastCalledWith()
    expect(s.testingAll).toBe(false)
  })

  it('导入后重新加载列表', async () => {
    const s = useProxyPoolStore()
    await s.load()
    h.api.importProxyNodes.mockResolvedValue({ added: 1, skipped: [{ line: 2, reason: 'x' }] })
    h.api.getProxyPool.mockResolvedValue(state({}, [node(), node({ id: 'n2' })]))
    const r = await s.importNodes('a:1')
    expect(r.added).toBe(1)
    expect(s.nodes).toHaveLength(2)
  })
})
