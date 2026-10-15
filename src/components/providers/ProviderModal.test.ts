import { beforeEach, describe, expect, it, vi } from 'vitest'
import { flushPromises, mount } from '@vue/test-utils'
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
  fetchProviderModels: vi.fn(),
  testProviderModel: vi.fn(),
}))
vi.mock('@/api', () => ({ api: apiMock }))

import BaseModal from '@/components/BaseModal.vue'
import ConfirmDialog from '@/components/ConfirmDialog.vue'
import ProviderModal from './ProviderModal.vue'

const summary: ProviderSummary = {
  id: 'ag',
  name: 'Antigravity',
  group: 'oauth',
  status: 'connected',
  connectionCount: 1,
  adapterReady: true,
  protocols: ['openai-chat'],
  quotaHint: null,
  serviceKinds: ['llm'],
  website: null,
  color: null,
  textIcon: null,
  authModes: ['apiKey'],
  pendingReason: null,
}

function detail(over: Partial<ProviderDetail> = {}): ProviderDetail {
  return {
    ...summary,
    riskNotice: '风险文案',
    connections: [
      { id: 'c1', providerId: 'ag', accountLabel: 'one@example.com', kind: 'oauth', status: 'active', priority: 1, weight: 1, enabled: true },
    ],
    distribution: { roundRobin: true, maxConcurrency: 4, maxRetries: 2 },
    customHeaders: [],
    models: [
      { id: 'ag/c1/a', upstreamModelId: 'a', publicAlias: 'ag/model-a', description: 'A', enabled: true, supportsThinking: true },
      { id: 'ag/c1/b', upstreamModelId: 'b', publicAlias: 'ag/model-b', description: 'B', enabled: true, supportsThinking: false },
    ],
    thinkingLevel: 'auto',
    ...over,
  }
}

async function open(d: ProviderDetail = detail(), prov: ProviderSummary = summary) {
  apiMock.getProvider.mockResolvedValue(d)
  const w = mount(ProviderModal, {
    props: { modelValue: true, provider: prov },
    global: { plugins: [createPinia()], stubs: { teleport: true } },
  })
  await flushPromises()
  return w
}

const saveBtn = (w: ReturnType<typeof mount>) =>
  w.findAll('button').find((b) => b.text().includes('保存修改'))!

beforeEach(() => {
  setActivePinia(createPinia())
  Object.values(apiMock).forEach((f) => f.mockReset())
  apiMock.listProviders.mockResolvedValue([])
})

describe('ProviderModal', () => {
  it('免密钥提供商一键启用，无 API Key 输入框，连接显示免密钥', async () => {
    const free = { ...summary, id: 'opencode', name: 'OpenCode Free', group: 'free' as const, status: 'disconnected' as const,
      connectionCount: 0, authModes: ['none' as const], protocols: ['openai-chat' as const] }
    const w = await open(detail({ ...free, connections: [] }), free)
    expect(w.find('button').exists()).toBe(true)
    expect(w.text()).toContain('一键启用')
    expect(w.find('input[aria-label="API Key"]').exists()).toBe(false)
    const conn = { id: 'no-key-1', providerId: 'opencode', accountLabel: '免密钥连接', kind: 'none',
      status: 'active', priority: 1, weight: 1, enabled: true }
    apiMock.addConnection.mockResolvedValue(conn)
    await w.findAll('button').find((b) => b.text().includes('一键启用'))!.trigger('click')
    await flushPromises()
    expect(apiMock.addConnection).toHaveBeenCalledWith({ providerId: 'opencode', accountLabel: '免密钥连接', apiKey: '' })
    expect(w.text()).toContain('免密钥连接已启用')
    expect(w.findAll('button').some((b) => b.text().includes('添加 API Key'))).toBe(false)
  })
  it('starts clean: not dirty, save disabled', async () => {
    const w = await open()
    expect(w.findComponent(BaseModal).props('dirty')).toBe(false)
    expect(saveBtn(w).attributes('disabled')).toBeDefined()
    expect(saveBtn(w).classes()).not.toContain('btn-primary')
  })

  it('becomes dirty after an edit and highlights save', async () => {
    const w = await open()
    await w.find('input[aria-label="启用模型 ag/model-b"]').setValue(false)
    expect(w.findComponent(BaseModal).props('dirty')).toBe(true)
    expect(saveBtn(w).attributes('disabled')).toBeUndefined()
    expect(saveBtn(w).classes()).toContain('btn-primary')
    // 改回去后恢复为未修改
    await w.find('input[aria-label="启用模型 ag/model-b"]').setValue(true)
    expect(w.findComponent(BaseModal).props('dirty')).toBe(false)
  })

  it('blocks saving on alias conflict and shows a message (no silent overwrite)', async () => {
    const w = await open()
    await w.find('input[aria-label="公开别名 b"]').setValue('ag/model-a')
    expect(w.text()).toContain('别名与其它已启用模型冲突')

    await saveBtn(w).trigger('click')
    await flushPromises()
    expect(apiMock.saveProvider).not.toHaveBeenCalled()
    expect(w.find('.issues').text()).toContain('模型别名冲突')
    expect(w.findComponent(BaseModal).props('dirty')).toBe(true)
  })

  it('allows saving once the conflict is resolved, and re-fetches via the store', async () => {
    const w = await open()
    await w.find('input[aria-label="公开别名 b"]').setValue('ag/model-a')
    await saveBtn(w).trigger('click')
    expect(apiMock.saveProvider).not.toHaveBeenCalled()

    await w.find('input[aria-label="公开别名 b"]').setValue('ag/model-b2')
    apiMock.saveProvider.mockImplementation(async (d: ProviderDetail) => d)
    await saveBtn(w).trigger('click')
    await flushPromises()
    expect(apiMock.saveProvider).toHaveBeenCalledTimes(1)
    expect(apiMock.saveProvider.mock.calls[0][0].models[1].publicAlias).toBe('ag/model-b2')
    expect(apiMock.listProviders).toHaveBeenCalledTimes(1)
    expect(w.emitted('update:modelValue')?.at(-1)).toEqual([false])
  })

  it('does not count disabled models in conflicts', async () => {
    const w = await open()
    await w.find('input[aria-label="启用模型 ag/model-b"]').setValue(false)
    await w.find('input[aria-label="公开别名 b"]').setValue('ag/model-a')
    await saveBtn(w).trigger('click')
    await flushPromises()
    expect(apiMock.saveProvider).toHaveBeenCalledTimes(1)
  })

  it('shows save errors and stays open', async () => {
    const w = await open()
    await w.find('input[aria-label="启用模型 ag/model-b"]').setValue(false)
    apiMock.saveProvider.mockRejectedValue(new Error('disk full'))
    await saveBtn(w).trigger('click')
    await flushPromises()
    expect(w.text()).toContain('保存失败：disk full')
    expect(w.emitted('update:modelValue')).toBeUndefined()
  })

  it('marks models without thinking support explicitly', async () => {
    const w = await open()
    expect(w.text()).toContain('不支持思考')
  })

  it('shows risk notice only when provided and not for custom group', async () => {
    const w = await open()
    expect(w.text()).toContain('风险提示')
    const none = await open(detail({ riskNotice: null }))
    expect(none.text()).not.toContain('风险提示')
    const custom = await open(detail({ group: 'custom' }), { ...summary, group: 'custom' })
    expect(custom.text()).not.toContain('风险提示')
  })

  it('treats adapterReady=false as read-only with 待接入 notice', async () => {
    const d = detail({ adapterReady: false, status: 'unsupported', connections: [], models: [] })
    const w = await open(d, { ...summary, adapterReady: false, status: 'unsupported' })
    expect(w.text()).toContain('待接入')
    expect(w.text()).not.toContain('已连接')
    const add = w.findAll('button').find((b) => b.text().includes('添加 API Key'))
    expect(add).toBeUndefined()
  })

  it('requires confirmation before removing a connection, and removal marks dirty', async () => {
    const w = await open(
      detail({
        connections: [
          { id: 'c1', providerId: 'ag', accountLabel: 'one@example.com', kind: 'apiKey', status: 'active', priority: 1, weight: 1, enabled: true },
        ],
      }),
    )
    await w.find('button[aria-label="删除连接 one@example.com"]').trigger('click')
    // 尚未确认：连接仍在，未修改
    expect(w.findComponent(BaseModal).props('dirty')).toBe(false)
    const confirm = w.findComponent(ConfirmDialog).findAll('button').find((b) => b.text() === '删除')!
    await confirm.trigger('click')
    expect(w.find('button[aria-label="删除连接 one@example.com"]').exists()).toBe(false)
    expect(w.findAllComponents(BaseModal)[0].props('dirty')).toBe(true)
  })

  it('shows load failure with retry', async () => {
    apiMock.getProvider.mockRejectedValueOnce(new Error('offline'))
    const w = mount(ProviderModal, {
      props: { modelValue: true, provider: summary },
      global: { plugins: [createPinia()], stubs: { teleport: true } },
    })
    await flushPromises()
    expect(w.text()).toContain('加载失败：offline')
    apiMock.getProvider.mockResolvedValue(detail())
    await w.findAll('button').find((b) => b.text() === '重试')!.trigger('click')
    await flushPromises()
    expect(w.text()).toContain('one@example.com')
  })

  it('masks secret header values', async () => {
    const w = await open(detail({ customHeaders: [{ key: 'X-Token', value: 'abc', secret: true }] }))
    const toggle = w.findAll('button').find((b) => b.text().includes('Custom Headers'))!
    await toggle.trigger('click')
    expect(w.find('input[aria-label="Header 值 1"]').attributes('type')).toBe('password')
  })

  // ---- 模型级可用性探测 ----

  const modelTestBtn = (w: ReturnType<typeof mount>, alias: string) =>
    w.findAll('button').find((b) => b.attributes('aria-label') === `测试模型可用性 ${alias}`)

  it('每个模型行都有“测试”按钮，点击后调用后端并把成功结果显示出来', async () => {
    apiMock.testProviderModel.mockResolvedValue({
      connectionId: '',
      providerId: 'ag',
      ok: true,
      latencyMs: 42,
      error: null,
    })
    const w = await open()

    const btn = modelTestBtn(w, 'ag/model-a')
    expect(btn).toBeTruthy()
    await btn!.trigger('click')
    await flushPromises()

    expect(apiMock.testProviderModel).toHaveBeenCalledWith('ag', 'ag/c1/a')
    const note = w.find('[data-testid="model-test-note"]')
    expect(note.exists()).toBe(true)
    expect(note.text()).toContain('ag/model-a')
    expect(note.text()).toContain('可用')
    expect(note.text()).toContain('42ms')
    // 行内按钮本身也变成可用状态（含延迟）
    const row = modelTestBtn(w, 'ag/model-a')!
    expect(row.classes()).toContain('ok')
    expect(row.text()).toContain('42ms')
  })

  it('探测失败时就地显示原因，且不把它当成成功', async () => {
    apiMock.testProviderModel.mockResolvedValue({
      connectionId: '',
      providerId: 'ag',
      ok: false,
      latencyMs: null,
      error: '上游找不到该模型（HTTP 404）：model not found',
    })
    const w = await open()

    await modelTestBtn(w, 'ag/model-b')!.trigger('click')
    await flushPromises()

    const note = w.find('[data-testid="model-test-note"]')
    expect(note.text()).toContain('探测失败')
    expect(note.text()).toContain('找不到该模型')
    // 失败的模型行内显示原因，且按钮标记为失败而不是可用
    expect(w.find('[data-test="model-test-error"]').text()).toContain('找不到该模型')
    expect(modelTestBtn(w, 'ag/model-b')!.classes()).toContain('bad')
  })

  it('API 抛错时也如实报告失败，不会静默', async () => {
    apiMock.testProviderModel.mockRejectedValue(new Error('网络中断'))
    const w = await open()

    await modelTestBtn(w, 'ag/model-a')!.trigger('click')
    await flushPromises()

    expect(w.find('[data-testid="model-test-note"]').text()).toContain('网络中断')
  })

  it('适配器未接入（只读）时“测试”按钮禁用', async () => {
    const w = await open(detail({ adapterReady: false, pendingReason: '适配器尚未接入' }))
    const btn = modelTestBtn(w, 'ag/model-a')
    expect(btn!.attributes('disabled')).toBeDefined()
  })

  it('换提供商时清掉上一轮的探测结论，避免展示过期结果', async () => {
    apiMock.testProviderModel.mockResolvedValue({
      connectionId: '',
      providerId: 'ag',
      ok: true,
      latencyMs: 12,
      error: null,
    })
    const w = await open()
    await modelTestBtn(w, 'ag/model-a')!.trigger('click')
    await flushPromises()
    expect(w.find('[data-testid="model-test-note"]').exists()).toBe(true)

    // 切换到另一个提供商（id 变化会触发重新 load）后旧的探测结论应被清除
    const other = detail({ id: 'other', name: 'Other' })
    apiMock.getProvider.mockResolvedValue(other)
    await w.setProps({ provider: { ...summary, id: 'other', name: 'Other' } })
    await flushPromises()
    expect(w.find('[data-testid="model-test-note"]').exists()).toBe(false)
  })
})
