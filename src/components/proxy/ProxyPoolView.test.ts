import { flushPromises, mount, type VueWrapper } from '@vue/test-utils'
import { createPinia, setActivePinia } from 'pinia'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import type { ProxyNode, ProxyPoolConfig, ProxyPoolState } from '@/types'

const h = vi.hoisted(() => ({
  api: {
    getProxyPool: vi.fn(), saveProxyPoolConfig: vi.fn(), saveProxyNode: vi.fn(), deleteProxyNode: vi.fn(),
    setProxyNodeEnabled: vi.fn(), moveProxyNode: vi.fn(), importProxyNodes: vi.fn(), testProxyNodes: vi.fn(),
    listProviders: vi.fn(),
  },
}))
vi.mock('@/api', () => ({ api: h.api }))

import ProxyPoolView from '@/views/ProxyPoolView.vue'

const config: ProxyPoolConfig = {
  enabled: false, strategy: 'roundRobin', providerScope: 'all', providerIds: [], failureThreshold: 3, cooldownSeconds: 60,
  maxAttempts: 2, fallbackDirect: false, testUrl: 'https://example.com/trace',
}
const node = (over: Partial<ProxyNode> = {}): ProxyNode => ({
  id: 'n1', name: 'HK-1', scheme: 'socks5', host: '10.0.0.1', port: 1080, username: 'alice', hasPassword: true, enabled: true,
  status: 'healthy', latencyMs: 120, lastCheckedAt: null, lastError: null, exitIp: '203.0.113.5', successCount: 3, failureCount: 1,
  cooldownRemainingSeconds: 0, ...over,
})
const state = (c: Partial<ProxyPoolConfig> = {}, nodes: ProxyNode[] = [node()]): ProxyPoolState => ({ config: { ...config, ...c }, nodes })

const q = (sel: string) => document.body.querySelector<HTMLElement>(sel)
let wrapper: VueWrapper

async function mountView(s: ProxyPoolState) {
  h.api.getProxyPool.mockResolvedValue(s)
  wrapper = mount(ProxyPoolView, { attachTo: document.body })
  await flushPromises()
}

beforeEach(() => {
  setActivePinia(createPinia())
  Object.values(h.api).forEach((f) => f.mockReset())
  h.api.listProviders.mockResolvedValue([])
})
afterEach(() => wrapper?.unmount())

describe('ProxyPoolView', () => {
  it('展示节点、地址与状态，密码只显示掩码', async () => {
    await mountView(state())
    const row = q('[data-testid="row-n1"]')!
    expect(row.textContent).toContain('socks5://10.0.0.1:1080')
    expect(row.textContent).toContain('alice')
    expect(row.textContent).toContain('••••')
    expect(row.textContent).toContain('正常')
    expect(row.textContent).toContain('120 ms')
    expect(row.textContent).toContain('203.0.113.5')
    expect(q('[data-testid="overview-stats"]')!.textContent).toContain('1 个节点 · 1 个可用')
  })

  it('没有节点时显示空状态', async () => {
    await mountView(state({}, []))
    expect(document.body.textContent).toContain('还没有代理节点')
    expect(q('[data-testid="test-all"]')!.hasAttribute('disabled')).toBe(true)
  })

  it('代理生效范围支持仅提供媒体能力的供应商', async () => {
    h.api.listProviders.mockResolvedValue([
      { id: 'image-only', name: 'Image Provider', serviceKinds: ['image'] },
      { id: 'voice-only', name: 'Voice Provider', serviceKinds: ['tts'] },
    ])
    await mountView(state({ providerScope: 'include' }))
    expect(q('[data-testid="pick-image-only"]')).not.toBeNull()
    expect(q('[data-testid="pick-voice-only"]')).not.toBeNull()
    q('[data-testid="pick-image-only"]')!.click()
    await flushPromises()
    h.api.saveProxyPoolConfig.mockResolvedValue(state({ providerScope: 'include', providerIds: ['image-only'] }))
    q('[data-testid="save-config"]')!.click()
    await flushPromises()
    expect(h.api.saveProxyPoolConfig).toHaveBeenCalledWith({ ...config, providerScope: 'include', providerIds: ['image-only'] })
  })

  it('开启但无可用节点：按是否回退直连显示不同提示', async () => {
    await mountView(state({ enabled: true }, [node({ enabled: false })]))
    expect(q('[data-testid="no-usable-banner"]')!.textContent).toContain('会被拒绝')
    wrapper.unmount()
    await mountView(state({ enabled: true, fallbackDirect: true }, []))
    expect(q('[data-testid="no-usable-banner"]')!.textContent).toContain('回退为直连')
  })

  it('总开关调用保存配置', async () => {
    await mountView(state())
    h.api.saveProxyPoolConfig.mockResolvedValue(state({ enabled: true }))
    q('[data-testid="pool-toggle"]')!.click()
    await flushPromises()
    expect(h.api.saveProxyPoolConfig).toHaveBeenCalledWith({ ...config, enabled: true })
  })

  it('修改配置后才能保存', async () => {
    await mountView(state())
    const save = q('[data-testid="save-config"]') as HTMLButtonElement
    expect(save.disabled).toBe(true)
    const sel = q('[data-testid="strategy"]') as HTMLSelectElement
    sel.value = 'failover'
    sel.dispatchEvent(new Event('change'))
    await flushPromises()
    expect(save.disabled).toBe(false)
    h.api.saveProxyPoolConfig.mockResolvedValue(state({ strategy: 'failover' }))
    save.click()
    await flushPromises()
    expect(h.api.saveProxyPoolConfig).toHaveBeenCalledWith({ ...config, strategy: 'failover' })
  })

  it('保存失败时显示后端错误', async () => {
    await mountView(state())
    const sel = q('[data-testid="strategy"]') as HTMLSelectElement
    sel.value = 'random'
    sel.dispatchEvent(new Event('change'))
    await flushPromises()
    h.api.saveProxyPoolConfig.mockRejectedValue('冷却时间需在 5–3600 秒之间')
    q('[data-testid="save-config"]')!.click()
    await flushPromises()
    expect(q('[data-testid="config-error"]')!.textContent).toContain('冷却时间')
  })

  it('范围选“仅指定”时出现提供商选择器', async () => {
    await mountView(state())
    expect(q('[data-testid="provider-picker"]')).toBeNull()
    const sel = q('[data-testid="scope"]') as HTMLSelectElement
    sel.value = 'include'
    sel.dispatchEvent(new Event('change'))
    await flushPromises()
    expect(q('[data-testid="provider-picker"]')).not.toBeNull()
  })

  it('测速、停用、上下移动、删除都走对应接口', async () => {
    await mountView(state({}, [node(), node({ id: 'n2', name: 'JP-1' })]))
    h.api.testProxyNodes.mockResolvedValue([node(), node({ id: 'n2', name: 'JP-1' })])
    q('[data-testid="test-n1"]')!.click()
    await flushPromises()
    expect(h.api.testProxyNodes).toHaveBeenCalledWith(['n1'])

    h.api.setProxyNodeEnabled.mockResolvedValue(state({}, [node({ enabled: false }), node({ id: 'n2', name: 'JP-1' })]))
    q('[data-testid="toggle-n1"]')!.click()
    await flushPromises()
    expect(h.api.setProxyNodeEnabled).toHaveBeenCalledWith('n1', false)

    h.api.moveProxyNode.mockResolvedValue(state())
    expect((q('[data-testid="up-n1"]') as HTMLButtonElement).disabled).toBe(true)
    q('[data-testid="down-n1"]')!.click()
    await flushPromises()
    expect(h.api.moveProxyNode).toHaveBeenCalledWith('n1', 1)

    h.api.deleteProxyNode.mockResolvedValue(state({}, [node({ id: 'n2', name: 'JP-1' })]))
    q('[data-testid="delete-n1"]')!.click()
    await flushPromises()
    ;[...document.body.querySelectorAll('button')].find((b) => b.textContent?.trim() === '确认删除')!.click()
    await flushPromises()
    expect(h.api.deleteProxyNode).toHaveBeenCalledWith('n1')
  })

  it('操作失败时显示错误', async () => {
    await mountView(state())
    h.api.setProxyNodeEnabled.mockRejectedValue('数据库忙')
    q('[data-testid="toggle-n1"]')!.click()
    await flushPromises()
    expect(q('[data-testid="action-error"]')!.textContent).toContain('数据库忙')
  })

  it('批量导入：展示新增数量与被跳过的行（只含行号与原因）', async () => {
    await mountView(state({}, []))
    h.api.importProxyNodes.mockResolvedValue({ added: 2, skipped: [{ line: 3, reason: '格式无法识别' }] })
    q('[data-testid="open-import"]')!.click()
    await flushPromises()
    const ta = q('[data-testid="import-text"]') as HTMLTextAreaElement
    ta.value = 'socks5://a:b@h:1\nh2:2\nbad'
    ta.dispatchEvent(new Event('input'))
    await flushPromises()
    q('[data-testid="submit-import"]')!.click()
    await flushPromises()
    expect(h.api.importProxyNodes).toHaveBeenCalledWith('socks5://a:b@h:1\nh2:2\nbad')
    expect(q('[data-testid="import-summary"]')!.textContent).toContain('已导入 2 个节点，跳过 1 行')
    expect(q('[data-testid="import-skipped"]')!.textContent).toContain('第 3 行：格式无法识别')
  })

  it('添加节点：校验失败不提交，成功后提交规整后的输入', async () => {
    await mountView(state({}, []))
    q('[data-testid="open-add"]')!.click()
    await flushPromises()
    q('[data-testid="submit-node"]')!.click()
    await flushPromises()
    expect(h.api.saveProxyNode).not.toHaveBeenCalled()
    expect(document.body.textContent).toContain('请填写代理主机')

    const set = async (id: string, v: string) => {
      const el = q(id) as HTMLInputElement
      el.value = v
      el.dispatchEvent(new Event('input'))
      await flushPromises()
    }
    await set('#px-host', 'proxy.example.com')
    await set('#px-port', '8080')
    h.api.saveProxyNode.mockResolvedValue(state({}, [node()]))
    q('[data-testid="submit-node"]')!.click()
    await flushPromises()
    expect(h.api.saveProxyNode).toHaveBeenCalledWith({
      name: 'proxy.example.com:8080', scheme: 'http', host: 'proxy.example.com', port: 8080, username: null, password: '', enabled: true,
    })
  })
})
